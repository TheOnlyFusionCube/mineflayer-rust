#!/usr/bin/env python3
"""G4 compatibility checks of mineflayer-presence against real server software.

    python3 compat.py [check ...]      # default: all

Checks (each prints "G4 <name> PASS|FAIL <detail>"):
  paper-<v>         Paper on 1.8.8, 1.12.2, 1.16.5, 1.20.4, 1.21.11, 26.1.2: the
                    full presence-matrix check
  via-<client>      Paper 26.2 with ViaVersion, ViaBackwards, ViaRewind, joined
                    by a 1.7.10, 1.8.9, 1.12.2, 1.20.4 and 26.3 client
  transfer          vanilla 26.1 /transfer to a second server (1.20.5+)
  velocity-<v>      a Velocity server switch between two vanilla backends of v
  bungee-<v>        the same through BungeeCord
  client-<kind>-<v> the vanilla check with the bots behind a SOCKS5, SOCKS4 or
                    HTTP proxy that requires credentials (proxy_server.py)
  online-<v>        online mode against a mock Yggdrasil (yggdrasil.py) through
                    authlib-injector: encryption and the session-server join
  signed-<v>        the same with enforce-secure-profile=true: chat and /msg
                    must arrive signed (1.19.3+)

Env: VT (cache dir, default ~/.cache/presence-matrix), BIN (presence example),
PORT_BASE (first port, default 24000; checks take 100 ports each).
Exit status 1 when any check fails.
"""
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
VT = os.environ.get("VT", os.path.expanduser("~/.cache/presence-matrix"))
BIN = os.environ.get("BIN", os.path.join(HERE, "../../target/release/examples/presence"))
CHECK = os.path.join(HERE, "../presence-matrix/check.sh")
CACHE = os.path.join(VT, "compat")
UA = {"User-Agent": "mineflayer-presence-compat/1"}
os.makedirs(CACHE, exist_ok=True)
failed = False


def say(name, ok, detail):
    global failed
    failed |= not ok
    print(f"G4 {name} {'PASS' if ok else 'FAIL'} {detail}", flush=True)


def fetch(url, path):
    if not os.path.exists(path):
        with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as r, open(path + ".part", "wb") as f:
            shutil.copyfileobj(r, f)
        os.rename(path + ".part", path)
    return path


def fill(project, version):
    builds = json.load(urllib.request.urlopen(urllib.request.Request(
        f"https://fill.papermc.io/v3/projects/{project}/versions/{version}/builds", headers=UA), timeout=30))
    build = next(b for b in builds if b.get("channel") in ("STABLE", "RECOMMENDED"))
    url = build["downloads"]["server:default"]["url"]
    return fetch(url, os.path.join(CACHE, f"{project}-{version}-{build['id']}.jar"))


def github(repo):
    rel = json.load(urllib.request.urlopen(urllib.request.Request(
        f"https://api.github.com/repos/{repo}/releases/latest", headers=UA), timeout=30))
    asset = next(a for a in rel["assets"] if a["name"].endswith(".jar"))
    return fetch(asset["browser_download_url"], os.path.join(CACHE, asset["name"]))


def jre(major):
    java = os.path.join(VT, "jre", str(major), "bin", "java")
    if not os.path.exists(java):
        d = os.path.join(VT, "jre", str(major))
        os.makedirs(d, exist_ok=True)
        subprocess.run(f"curl -sSL https://api.adoptium.net/v3/binary/latest/{major}/ga/linux/x64/jre/hotspot/normal/eclipse"
                       f" | tar xz -C {d} --strip-components=1", shell=True, check=True)
    return java


def vanilla(version):
    """The vanilla server jar and Java major check.sh caches for a release."""
    d = os.path.join(VT, "servers", version)
    if not os.path.exists(os.path.join(d, "server.jar")):
        subprocess.run([CHECK, version, "29990"], capture_output=True)
    return os.path.join(d, "server.jar"), int(open(os.path.join(d, "javamajor")).read())


def rcon(port, *commands):
    out = []
    s = socket.create_connection(("127.0.0.1", port), timeout=10)

    def pkt(i, t, body):
        b = struct.pack("<ii", i, t) + body.encode() + b"\0\0"
        s.sendall(struct.pack("<i", len(b)) + b)
        n = struct.unpack("<i", s.recv(4))[0]
        data = b""
        while len(data) < n:
            data += s.recv(n - len(data))
        return data[8:-2].decode(errors="replace")

    pkt(1, 3, "pres")
    for c in commands:
        out.append(pkt(2, 2, c))
        time.sleep(0.3)
    s.close()
    return out


def wait_for(path, needle, seconds, proc=None):
    for _ in range(seconds):
        if os.path.exists(path) and needle in open(path, errors="replace").read():
            return True
        if proc is not None and proc.poll() is not None:
            break
        time.sleep(1)
    raise RuntimeError(f"{needle!r} never appeared in {path}")


class Server:
    """A vanilla backend in its own directory, offline, flat, with RCON."""

    def __init__(self, name, version, port, extra=""):
        self.dir = os.path.join(CACHE, "run", name)
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(self.dir)
        jar, major = vanilla(version)
        shutil.copy(jar, os.path.join(self.dir, "server.jar"))
        flat = "minecraft\\:flat" if tuple(map(int, version.split("."))) >= (1, 19) else "FLAT"
        open(os.path.join(self.dir, "eula.txt"), "w").write("eula=true\n")
        open(os.path.join(self.dir, "server.properties"), "w").write(
            f"server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\nenforce-secure-profile=false\n"
            f"white-list=false\nview-distance=4\nsimulation-distance=4\nspawn-protection=0\ndifficulty=peaceful\n"
            f"level-type={flat}\ngenerate-structures=false\nspawn-monsters=false\nenable-rcon=true\n"
            f"rcon.port={port + 1000}\nrcon.password=pres\n{extra}")
        self.port, self.rcon_port = port, port + 1000
        self.log = os.path.join(self.dir, "server.out")
        self.proc = subprocess.Popen([jre(major), "-Xmx1g", "-Xms256m", "-jar", "server.jar", "nogui"],
                                     cwd=self.dir, stdin=subprocess.DEVNULL, stdout=open(self.log, "w"), stderr=subprocess.STDOUT)
        wait_for(self.log, "Done (", 240, self.proc)

    def stop(self):
        try:
            rcon(self.rcon_port, "stop")
            self.proc.wait(30)
        except Exception:
            self.proc.kill()


def client(port, count, seconds, *flags):
    return subprocess.Popen([BIN, "127.0.0.1", str(port), str(count), str(seconds), *flags],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)


def finish(p):
    out, _ = p.communicate(timeout=300)
    return p.returncode, out


def run_check(name, version, port, **env):
    full = dict(os.environ, VT=VT, BIN=BIN, NAME=name, **{k: str(v) for k, v in env.items()})
    r = subprocess.run([CHECK, version, str(port)], env=full, capture_output=True, text=True)
    lines = r.stdout.strip().splitlines()
    verdict = lines[-1] if lines else "no output"
    reasons = [l for l in lines if l.startswith("reason:")]
    say(name, verdict.startswith("VERDICT PASS"), verdict if not reasons else "; ".join(reasons)[:300])


# Paper 1.16.5 refuses Java 17 ("Only up to Java 16 is supported")
PAPER = {"1.8.8": 8, "1.12.2": 8, "1.16.5": 11, "1.20.4": 17, "1.21.11": 21, "26.1.2": 25}
VIA_CLIENTS = {"1.7.10": 5, "1.8.9": 47, "1.12.2": 340, "1.20.4": 765, "26.3": 777}


# a proxy answers the status ping with its own version range, so the bot is
# told the backend's protocol
PROXY_PROTOCOL = {"1.8.9": 47, "1.20.4": 765, "26.1": 775}


def check_paper(v, port):
    run_check(f"paper-{v}", v, port, SERVER_JAR=fill("paper", v), JAVA_MAJOR=PAPER[v])


def check_via(client_version, port):
    plugins = " ".join(github(f"ViaVersion/{r}") for r in ["ViaVersion", "ViaBackwards", "ViaRewind"])
    run_check(f"via-{client_version}", "26.2", port, SERVER_JAR=fill("paper", "26.2"), JAVA_MAJOR=25,
              PLUGINS=plugins, CLIENT_PROTOCOL=VIA_CLIENTS[client_version])


PROXIES = ["socks5", "socks4", "http"]


def check_proxy_client(kind, version, port):
    """The full vanilla check with both bots tunnelled through a proxy that
    requires credentials (proxy_server.py)."""
    log = os.path.join(CACHE, f"proxy-{kind}-{version}.log")
    proxy = subprocess.Popen([sys.executable, os.path.join(HERE, "proxy_server.py"), kind, str(port + 50), "pres", "s3cret"],
                             stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        time.sleep(1)
        auth = "pres@" if kind == "socks4" else "pres:s3cret@"
        run_check(f"client-{kind}-{version}", version, port, CLIENT_PROXY=f"{kind}://{auth}127.0.0.1:{port + 50}")
        tunnels = open(log).read().count("tunnel ")
        if tunnels < 3:
            say(f"client-{kind}-{version}", False, f"only {tunnels} tunnels through the proxy (ping and two bots expected)")
    finally:
        proxy.kill()


ONLINE = ["1.8.9", "1.12.2", "1.16.5", "1.20.4", "26.1"]
# enforce-secure-profile=true: chat and /msg must arrive signed (1.19.3+)
SIGNED = ["1.19.3", "1.20.4", "1.20.6", "1.21.5", "26.1", "26.3"]


def check_online(v, port, enforce=False):
    mock = subprocess.Popen([sys.executable, os.path.join(HERE, "yggdrasil.py"), str(port + 50)],
                            stdout=subprocess.DEVNULL, stderr=open(os.path.join(CACHE, f"yggdrasil-{v}.log"), "w"))
    try:
        time.sleep(2)
        extra = {"ENFORCE": 1} if enforce else {}
        run_check(f"{'signed' if enforce else 'online'}-{v}", v, port, YGGDRASIL=f"http://127.0.0.1:{port + 50}",
                  AUTHLIB_INJECTOR=github("yushijinhun/authlib-injector"), **extra)
    finally:
        mock.kill()


def check_transfer(port):
    a = Server("transfer-a", "26.1", port, "accepts-transfers=true\n")
    b = Server("transfer-b", "26.1", port + 20, "accepts-transfers=true\n")
    try:
        p = client(port, 1, 60)
        wait_for(a.log, "pres000 joined the game", 60)
        # vanilla: transfer <hostname> [<port>] [<players>]
        reply = rcon(a.rcon_port, f"transfer 127.0.0.1 {b.port} pres000")
        print(f"  transfer reply: {reply}", flush=True)
        wait_for(b.log, "pres000 joined the game", 60)
        time.sleep(3)
        rcon(b.rcon_port, "say after-transfer")
        code, out = finish(p)
        heard = "after-transfer" in out
        say("transfer", code == 0 and heard, f"joined b; {'chat on b reached the bot' if heard else 'no chat from b'}; exit {code}")
    except Exception as e:
        say("transfer", False, f"{type(e).__name__}: {e}"[:200])
    finally:
        a.stop()
        b.stop()


def check_proxy(kind, version, port):
    name = f"{kind}-{version}"
    a = Server(f"{name}-a", version, port + 1)
    b = Server(f"{name}-b", version, port + 21)
    pdir = os.path.join(CACHE, "run", name)
    os.makedirs(pdir, exist_ok=True)
    proc = None
    try:
        if kind == "velocity":
            jar = fill("velocity", "3.5.1")
            open(os.path.join(pdir, "velocity.toml"), "w").write(
                f'config-version = "2.7"\nbind = "127.0.0.1:{port}"\nonline-mode = false\n'
                f'player-info-forwarding-mode = "none"\nforce-key-authentication = false\n'
                f'[servers]\na = "127.0.0.1:{a.port}"\nb = "127.0.0.1:{b.port}"\ntry = ["a"]\n'
                f'[forced-hosts]\n[advanced]\n[query]\nenabled = false\n')
            ready = "Done ("
        else:
            jar = fetch("https://ci.md-5.net/job/BungeeCord/lastSuccessfulBuild/artifact/bootstrap/target/BungeeCord.jar",
                        os.path.join(CACHE, "BungeeCord.jar"))
            open(os.path.join(pdir, "config.yml"), "w").write(
                "online_mode: false\nip_forward: false\nenforce_secure_profile: false\n"
                "servers:\n"
                f"  lobby:\n    address: 127.0.0.1:{a.port}\n    restricted: false\n    motd: a\n"
                f"  b:\n    address: 127.0.0.1:{b.port}\n    restricted: false\n    motd: b\n"
                "listeners:\n"
                f"- host: 127.0.0.1:{port}\n  query_enabled: false\n  motd: presence\n  max_players: 10\n"
                "  priorities:\n  - lobby\n  force_default_server: false\n  tab_list: GLOBAL_PING\n  ping_passthrough: false\n"
                "permissions:\n  default:\n  - bungeecord.command.server\n"
                "groups: {}\n")
            ready = "Listening on"
        log = os.path.join(pdir, "proxy.out")
        proc = subprocess.Popen([jre(21), "-Xmx512m", "-jar", jar], cwd=pdir, stdin=subprocess.PIPE,
                                stdout=open(log, "w"), stderr=subprocess.STDOUT)
        wait_for(log, ready, 120, proc)
        p = client(port, 1, 70, f"--protocol={PROXY_PROTOCOL[version]}", "--then=/server b")
        wait_for(a.log, "pres000 joined the game", 60)
        wait_for(b.log, "pres000 joined the game", 60)
        time.sleep(4)
        rcon(b.rcon_port, "say after-switch")
        code, out = finish(p)
        heard = "after-switch" in out
        say(name, code == 0 and heard, f"switched a to b; {'chat on b reached the bot' if heard else 'no chat from b'}; exit {code}")
    except Exception as e:
        say(name, False, f"{type(e).__name__}: {e}"[:200])
    finally:
        if proc:
            try:
                proc.stdin.write(b"end\n" if kind == "bungee" else b"shutdown\n")
                proc.stdin.flush()
                proc.wait(20)
            except Exception:
                proc.kill()
        a.stop()
        b.stop()


# Offsets a check may bind from its base port: servers at +0, +1, +20, +21,
# their RCON 1000 above, the mock Yggdrasil at +50.
OFFSETS = [0, 1, 20, 21, 50, 1000, 1001, 1020, 1021]


def free_base(start):
    """The first base port at or after `start` whose whole block is free.
    Other services on a shared host hold arbitrary ports."""
    port = start
    while port < 31000:
        try:
            for off in OFFSETS:
                with socket.socket() as sock:
                    sock.bind(("127.0.0.1", port + off))
            return port
        except OSError:
            port += 100
    raise RuntimeError("no free port block below 32768")


def main():
    checks = []
    port = int(os.environ.get("PORT_BASE", 24000))
    for v in PAPER:
        checks.append((f"paper-{v}", lambda v=v, p=port: check_paper(v, free_base(p))))
        port += 100
    for c in VIA_CLIENTS:
        checks.append((f"via-{c}", lambda c=c, p=port: check_via(c, free_base(p))))
        port += 100
    checks.append(("transfer", lambda p=port: check_transfer(free_base(p))))
    port += 100
    for v in ONLINE:
        checks.append((f"online-{v}", lambda v=v, p=port: check_online(v, free_base(p))))
        port += 100
    for v in SIGNED:
        checks.append((f"signed-{v}", lambda v=v, p=port: check_online(v, free_base(p), enforce=True)))
        port += 100
    for kind in PROXIES:
        for v in ["1.8.9", "26.1"]:
            checks.append((f"client-{kind}-{v}", lambda k=kind, v=v, p=port: check_proxy_client(k, v, free_base(p))))
            port += 100
    for kind in ["velocity", "bungee"]:
        for v in PROXY_PROTOCOL:
            checks.append((f"{kind}-{v}", lambda k=kind, v=v, p=port: check_proxy(k, v, free_base(p))))
            port += 100
    wanted = sys.argv[1:]
    for name, fn in checks:
        if not wanted or any(name == w or name.startswith(w) for w in wanted):
            try:
                fn()
            except Exception as e:
                say(name, False, f"harness error: {type(e).__name__}: {e}"[:200])
    sys.exit(1 if failed else 0)


main()
