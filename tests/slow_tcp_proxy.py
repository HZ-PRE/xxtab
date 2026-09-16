#!/usr/bin/env python3
"""Bounded slow-reader TCP proxy for the isolated fixture; NOT a netem RTT/loss model."""
import asyncio
import socket
import sys


async def main(port, target, rate):
    async def accepted(reader, writer):
        upstream = None
        try:
            remote_reader, upstream = await asyncio.open_connection('127.0.0.1', target, limit=4096)

            async def copy(source, dest):
                while data := await source.read(2048):
                    await asyncio.sleep(len(data) / rate)
                    dest.write(data)
                    await dest.drain()
                if dest.can_write_eof():
                    dest.write_eof()

            await asyncio.gather(copy(reader, upstream), copy(remote_reader, writer))
        except (ConnectionError, OSError):
            pass
        finally:
            writer.close()
            if upstream:
                upstream.close()

    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 16 * 1024)
    listener.bind(('0.0.0.0', port))
    listener.listen()
    listener.setblocking(False)
    async with await asyncio.start_server(accepted, sock=listener, limit=4096) as server:
        await server.serve_forever()


if __name__ == '__main__':
    asyncio.run(main(*(int(value) for value in sys.argv[1:])))
