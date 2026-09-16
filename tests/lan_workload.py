#!/usr/bin/env python3
"""TCP file transfer + small RPCs over the real WG fixture, no credentials logged."""
import hashlib
import json
import os
import socket
import statistics
import struct
import sys
import threading
import time

BLOCK = bytes(range(256)) * 256
SIZE = int(os.environ.get('XXTAB_LAN_BYTES', 16 * 1024 * 1024))
assert SIZE > 0 and SIZE % len(BLOCK) == 0


def exact(sock, size):
    result = bytearray()
    while len(result) < size:
        chunk = sock.recv(size - len(result))
        if not chunk:
            raise RuntimeError('unexpected TCP EOF')
        result.extend(chunk)
    return bytes(result)


def handle(sock):
    with sock:
        sock.settimeout(30)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        mode = exact(sock, 1)
        if mode == b'e':
            while data := sock.recv(64):
                sock.sendall(data)
        elif mode == b'd':
            for _ in range(SIZE // len(BLOCK)):
                sock.sendall(BLOCK)
        elif mode == b'u':
            digest = hashlib.sha256()
            remaining = SIZE
            while remaining:
                data = exact(sock, min(remaining, len(BLOCK)))
                digest.update(data)
                remaining -= len(data)
            sock.sendall(digest.digest())
        else:
            raise RuntimeError('bad test command')


def connect(address, mode):
    sock = socket.create_connection((address, 18090), timeout=30)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    sock.sendall(mode)
    return sock


def client(address):
    expected = hashlib.sha256(BLOCK * (SIZE // len(BLOCK))).digest()
    for mode in ('idle', 'upload', 'download'):
        done = threading.Event()
        result = {}

        def bulk():
            try:
                begin = time.monotonic()
                with connect(address, b'u' if mode == 'upload' else b'd') as sock:
                    if mode == 'upload':
                        for _ in range(SIZE // len(BLOCK)):
                            sock.sendall(BLOCK)
                        assert exact(sock, 32) == expected
                    else:
                        digest = hashlib.sha256()
                        remaining = SIZE
                        while remaining:
                            data = exact(sock, min(remaining, len(BLOCK)))
                            digest.update(data)
                            remaining -= len(data)
                        assert digest.digest() == expected
                result['mbps'] = SIZE * 8 / (time.monotonic() - begin) / 1e6
            except Exception as error:
                result['error'] = str(error)
            finally:
                done.set()

        latency = []
        with connect(address, b'e') as echo:
            if mode != 'idle':
                thread = threading.Thread(target=bulk)
                thread.start()
            else:
                done.set()
            deadline = time.monotonic() + 60
            while (len(latency) < (100 if mode == 'idle' else 1) or not done.is_set()) and time.monotonic() < deadline:
                data = struct.pack('<Q', len(latency)) * 4
                begin = time.monotonic()
                echo.sendall(data)
                assert exact(echo, len(data)) == data
                latency.append((time.monotonic() - begin) * 1000)
                # An interactive request every 10 ms, without saturating the echo flow.
                time.sleep(0.01)
        if mode != 'idle':
            thread.join(timeout=35)
            assert not thread.is_alive(), 'file transfer did not stop'
        assert 'error' not in result, result
        latency.sort()
        print('LAN ' + json.dumps(dict(mode=mode, samples=len(latency),
            p50_ms=statistics.median(latency), p99_ms=latency[(len(latency)-1)*99//100],
            **result)), flush=True)


if __name__ == '__main__':
    if sys.argv[1] == 'server':
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind((sys.argv[2], 18090))
            listener.listen()
            while True:
                sock, _ = listener.accept()
                threading.Thread(target=handle, args=(sock,), daemon=True).start()
    else:
        client(sys.argv[2])
