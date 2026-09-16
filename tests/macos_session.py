"""macOS CI only: actual WG interface, explicit stop and GUI-heartbeat loss.

Uses a local WebSocket sink and synthetic keys; no user profiles or remote VPN.
Requires bundled app dependencies and noninteractive sudo on the runner.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time


def private_write(path, data):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as file:
        file.write(data)
        temp = file.name
    os.replace(temp, path)


def snapshot(directory):
    try:
        return json.loads((directory / "snapshot.json").read_bytes())
    except (OSError, ValueError):
        return {}


def serve(listener):
    # Only the handshake and draining are needed: this tests lifecycle, not a
    # successful remote WG handshake or payload interoperability.
    connection, _ = listener.accept()
    with connection:
        connection.settimeout(45)
        request = b""
        while b"\r\n\r\n" not in request and len(request) < 16384:
            part = connection.recv(4096)
            if not part:
                return
            request += part
        headers = dict(line.split(b":", 1) for line in request.split(b"\r\n")[1:] if b":" in line)
        key = next(value.strip() for name, value in headers.items() if name.lower() == b"sec-websocket-key")
        accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
        connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
        try:
            while connection.recv(65536):
                pass
        except (OSError, TimeoutError):
            pass


def exercise(binary, orphan):
    with socket.socket() as listener, tempfile.TemporaryDirectory(prefix="xxtab-ci-") as temp:
        listener.bind(("127.0.0.1", 0)); listener.listen(1)
        thread = threading.Thread(target=serve, args=(listener,), daemon=True); thread.start()
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
            udp.bind(("127.0.0.1", 0)); port = udp.getsockname()[1]
        directory = Path(temp)
        key = base64.b64encode(bytes([1]) * 32).decode()
        draft = dict(name="CI", tunnel=f"server='ws://127.0.0.1:{listener.getsockname()[1]}'\nlisten='127.0.0.1:{port}'\n[wireguard]\nconfig='wg.conf'\n", wireguard=f"[Interface]\nPrivateKey={key}\nAddress=10.254.251.1/32\n[Peer]\nPublicKey={key}\nAllowedIPs=10.254.251.2/32\n")
        private_write(directory / "request.json", json.dumps(draft).encode())
        private_write(directory / "heartbeat", b"start")
        worker = subprocess.Popen(["sudo", "-n", str(binary), "macos-session", str(directory), str(os.getuid())], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 40
            while snapshot(directory).get("status") != "Connected":
                assert worker.poll() is None, "worker exited before interface became ready"
                assert time.monotonic() < deadline, "interface startup timed out"
                private_write(directory / "heartbeat", str(time.monotonic()).encode())
                time.sleep(0.2)
            mapping = Path(f"/var/run/wireguard/xxm{os.getuid()}.name")
            assert mapping.exists(), "interface mapping missing"
            if not orphan:
                private_write(directory / "stop", b"stop")
            assert worker.wait(timeout=35) == 0, "worker failed during shutdown"
            state = snapshot(directory)
            assert state.get("finished") and state.get("status") == "Idle", "final status missing"
            assert not mapping.exists(), "WireGuard interface mapping remained after stop"
            print("PASS:", "heartbeat loss cleanup" if orphan else "explicit stop cleanup")
        finally:
            if worker.poll() is None:
                private_write(directory / "stop", b"stop")
                worker.wait(timeout=35)
            worker.stderr.close()
            thread.join(timeout=2)


if __name__ == "__main__":
    assert sys.platform == "darwin" and os.getuid() != 0, "run as ordinary macOS CI user"
    executable = Path(sys.argv[1]).resolve(strict=True)
    subprocess.run(["sudo", "-n", "true"], check=True)
    exercise(executable, orphan=False)
    exercise(executable, orphan=True)
