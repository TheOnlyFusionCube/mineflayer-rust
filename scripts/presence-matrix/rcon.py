import socket, struct, sys, time
host, port, pw = "127.0.0.1", int(sys.argv[1]), "pres"
def pkt(i, t, body):
    b = struct.pack("<ii", i, t) + body.encode() + b"\x00\x00"
    return struct.pack("<i", len(b)) + b
def recv(s):
    n = struct.unpack("<i", s.recv(4))[0]
    data = b""
    while len(data) < n:
        data += s.recv(n - len(data))
    return data[8:-2].decode(errors="replace")
s = socket.create_connection((host, port), timeout=10)
s.sendall(pkt(1, 3, pw)); recv(s)
for cmd in sys.argv[2:]:
    s.sendall(pkt(2, 2, cmd)); print("rcon", cmd, "->", recv(s)[:200]); time.sleep(0.5)
