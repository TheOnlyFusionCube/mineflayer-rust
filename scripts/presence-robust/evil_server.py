"""evil_server.py <port>

A hostile Minecraft server for client robustness checks. It answers the status
ping with protocol 775, then for each login connection picks a mode by
connection number:
  0 random frames of random bytes, valid lengths up to 64 KiB, forever
  1 a frame header declaring 2^28 bytes, then a slow trickle
  2 login compression, then a compressed frame declaring 300 bytes that
    inflates to 64 MiB
  3 login success, then random bytes under real play packet ids, forever
  4 a 4-byte varint frame length
  5 login success, then configuration keepalives every 5 ms that never finish
    configuration: a well-formed server that never lets the player in
Never closes first.
"""
import asyncio, json, os, random, struct, sys, zlib

PORT = int(sys.argv[1])
conn_no = 0


def varint(n):
    out = b""
    n &= 0xFFFFFFFF
    while True:
        b = n & 0x7F
        n >>= 7
        out += bytes([b | (0x80 if n else 0)])
        if not n:
            return out


def frame(payload):
    return varint(len(payload)) + payload


async def read_varint(r):
    n, shift = 0, 0
    while True:
        b = (await r.readexactly(1))[0]
        n |= (b & 0x7F) << shift
        if not b & 0x80:
            return n
        shift += 7


async def read_frame(r):
    return await r.readexactly(await read_varint(r))


BOMB = zlib.compress(b"\0" * (64 << 20), 9)


async def handle(r, w):
    global conn_no
    try:
        hs = await read_frame(r)
        if hs[-1] == 1:
            await read_frame(r)
            status = json.dumps({"version": {"name": "26.1", "protocol": 775}, "players": {"max": 1, "online": 0}, "description": {"text": "evil"}})
            body = status.encode()
            w.write(frame(b"\x00" + varint(len(body)) + body))
            await w.drain()
            ping = await read_frame(r)
            w.write(frame(ping))
            await w.drain()
            return
        await read_frame(r)
        mode = conn_no % 6
        conn_no += 1
        if mode == 0:
            while True:
                w.write(frame(os.urandom(random.randint(0, 65536))))
                await w.drain()
                await asyncio.sleep(0.01)
        elif mode == 1:
            w.write(varint(1 << 28))
            while True:
                w.write(os.urandom(1024))
                await w.drain()
                await asyncio.sleep(0.05)
        elif mode == 2:
            w.write(frame(b"\x03" + varint(256)))
            w.write(frame(varint(300) + BOMB))
            await w.drain()
            await asyncio.sleep(3600)
        elif mode == 3:
            uuid = os.urandom(16)
            w.write(frame(b"\x02" + uuid + varint(4) + b"evil" + varint(0)))
            await w.drain()
            while True:
                w.write(frame(varint(random.randint(0, 0x90)) + os.urandom(random.randint(0, 512))))
                await w.drain()
                await asyncio.sleep(0.005)
        elif mode == 5:
            w.write(frame(b"\x02" + os.urandom(16) + varint(4) + b"evil" + varint(0)))
            await w.drain()
            # 26.1 configuration keep_alive is 0x04
            while True:
                w.write(frame(b"\x04" + os.urandom(8)))
                await w.drain()
                await asyncio.sleep(0.005)
        else:
            w.write(b"\xff\xff\xff\xff\x0f")
            await w.drain()
            await asyncio.sleep(3600)
    except (asyncio.IncompleteReadError, ConnectionError):
        pass
    finally:
        w.close()


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", PORT)
    async with server:
        await server.serve_forever()


asyncio.run(main())
