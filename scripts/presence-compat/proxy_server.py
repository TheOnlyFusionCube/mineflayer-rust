#!/usr/bin/env python3
"""proxy_server.py <kind> <port> <user> <pass>: a SOCKS5, SOCKS4 or HTTP CONNECT
proxy for compat checks. It requires the credentials (SOCKS5 user/password,
the SOCKS4 user id, HTTP Basic), refuses anything else, and prints one line
per tunnel: "tunnel <kind> <host>:<port>".
"""
import asyncio
import base64
import socket
import struct
import sys

KIND, PORT, USER, PASS = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]


async def pipe(r, w):
    try:
        while data := await r.read(65536):
            w.write(data)
            await w.drain()
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        w.close()


async def tunnel(cr, cw, host, port):
    sr, sw = await asyncio.open_connection(host, port)
    print(f"tunnel {KIND} {host}:{port}", flush=True)
    await asyncio.gather(pipe(cr, sw), pipe(sr, cw))


async def socks5(r, w):
    ver, n = await r.readexactly(2)
    methods = await r.readexactly(n)
    if ver != 5 or 2 not in methods:
        w.write(b"\x05\xff")
        return None
    w.write(b"\x05\x02")
    _, ulen = await r.readexactly(2)
    user = (await r.readexactly(ulen)).decode()
    plen = (await r.readexactly(1))[0]
    pw = (await r.readexactly(plen)).decode()
    if (user, pw) != (USER, PASS):
        w.write(b"\x01\x01")
        return None
    w.write(b"\x01\x00")
    _, cmd, _, atyp = await r.readexactly(4)
    if atyp == 1:
        host = socket.inet_ntoa(await r.readexactly(4))
    elif atyp == 3:
        host = (await r.readexactly((await r.readexactly(1))[0])).decode()
    else:
        host = socket.inet_ntop(socket.AF_INET6, await r.readexactly(16))
    port = struct.unpack(">H", await r.readexactly(2))[0]
    w.write(b"\x05\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack(">H", 0))
    return host, port


async def socks4(r, w):
    ver, cmd = await r.readexactly(2)
    port = struct.unpack(">H", await r.readexactly(2))[0]
    host = socket.inet_ntoa(await r.readexactly(4))
    user = (await r.readuntil(b"\0"))[:-1].decode()
    if ver != 4 or cmd != 1 or user != USER:
        w.write(b"\x00\x5b" + b"\0" * 6)
        return None
    w.write(b"\x00\x5a" + b"\0" * 6)
    return host, port


async def http(r, w):
    head = (await r.readuntil(b"\r\n\r\n")).decode()
    lines = head.split("\r\n")
    method, target, _ = lines[0].split(" ", 2)
    expected = "Basic " + base64.b64encode(f"{USER}:{PASS}".encode()).decode()
    auth = next((l.split(":", 1)[1].strip() for l in lines[1:] if l.lower().startswith("proxy-authorization:")), None)
    if method != "CONNECT" or auth != expected:
        w.write(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
        return None
    w.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
    host, port = target.rsplit(":", 1)
    return host.strip("[]"), int(port)


async def handle(r, w):
    try:
        dest = await {"socks5": socks5, "socks4": socks4, "http": http}[KIND](r, w)
        await w.drain()
        if dest is None:
            print(f"refused {KIND}", flush=True)
            w.close()
            return
        await tunnel(r, w, *dest)
    except (ConnectionError, asyncio.IncompleteReadError, ValueError):
        w.close()


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", PORT)
    async with server:
        await server.serve_forever()


asyncio.run(main())
