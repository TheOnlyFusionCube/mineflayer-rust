"""freeze_proxy.py <listen-port> <target-port> <freeze-after-s>

Forward TCP to 127.0.0.1:<target-port>. <freeze-after-s> seconds after a
client connects, stop forwarding in both directions and keep the sockets open:
a half-open connection, as when a NAT or proxy silently drops a flow. Prints
"freeze <t>" and "closed <t> <seconds after freeze>" per connection.
"""
import asyncio, sys, time

LISTEN, TARGET, AFTER = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3])


async def pump(r, w, frozen):
    try:
        while True:
            data = await r.read(65536)
            if not data:
                break
            if frozen.is_set():
                continue
            w.write(data)
            await w.drain()
    except Exception:
        pass


async def handle(cr, cw):
    sr, sw = await asyncio.open_connection("127.0.0.1", TARGET)
    frozen = asyncio.Event()
    start = time.time()

    async def freezer():
        await asyncio.sleep(AFTER)
        frozen.set()
        print(f"freeze {time.time():.1f}", flush=True)

    asyncio.create_task(freezer())
    # frozen data is read and dropped in both directions: neither side sees an
    # error or a close, as on a path that silently black-holes packets
    await asyncio.gather(pump(cr, sw, frozen), pump(sr, cw, frozen))
    t = time.time()
    print(f"closed {t:.1f} {t - start - AFTER:.1f}", flush=True)
    for w in (cw, sw):
        w.close()


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", LISTEN)
    async with server:
        await server.serve_forever()


asyncio.run(main())
