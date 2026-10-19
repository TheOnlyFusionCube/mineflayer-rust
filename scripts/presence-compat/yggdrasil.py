#!/usr/bin/env python3
"""yggdrasil.py <port>: a mock authlib-injector (Yggdrasil) API for online-mode
checks without a Microsoft account. A server runs it through
`-javaagent:authlib-injector.jar=http://127.0.0.1:<port>`; a client logs in
with access token `tok-<name>`, whose profile id is Java's
UUID.nameUUIDFromBytes("OfflinePlayer:<name>").

Endpoints:
  GET  /                                                metadata, signing key
  POST /sessionserver/session/minecraft/join            client joins a server
  GET  /sessionserver/session/minecraft/hasJoined       server checks the join
  GET  /sessionserver/session/minecraft/profile/<id>    profile lookup
  POST /api/profiles/minecraft                          name lookup (empty)
  GET  /minecraftservices/publickeys                    keys servers verify with
  POST /minecraftservices/player/certificates           chat-signing key pair

Certificates are signed as Mojang signs them: the v2 signature is SHA1withRSA
over the profile id (two big-endian longs), the expiry in epoch milliseconds,
and the X.509 DER of the player's public key.
"""
import base64
import datetime
import hashlib
import json
import struct
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa

PORT = int(sys.argv[1])
ROOT_KEY = rsa.generate_private_key(public_exponent=65537, key_size=4096)
ROOT_PUB_DER = ROOT_KEY.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
ROOT_PUB_PEM = ROOT_KEY.public_key().public_bytes(serialization.Encoding.PEM, serialization.PublicFormat.SubjectPublicKeyInfo).decode()
joins = {}


def offline_uuid(name):
    h = bytearray(hashlib.md5(f"OfflinePlayer:{name}".encode()).digest())
    h[6] = (h[6] & 0x0F) | 0x30
    h[8] = (h[8] & 0x3F) | 0x80
    return uuid.UUID(bytes=bytes(h))


def profile(name):
    return {"id": offline_uuid(name).hex, "name": name, "properties": []}


def certificate(name):
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pub_der = key.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
    now = datetime.datetime.now(datetime.timezone.utc)
    expires = now + datetime.timedelta(hours=48)
    expires_ms = int(expires.timestamp() * 1000)
    pid = offline_uuid(name)
    msb, lsb = pid.int >> 64, pid.int & ((1 << 64) - 1)
    v2 = ROOT_KEY.sign(struct.pack(">qqq", *(x - (1 << 64) if x >= 1 << 63 else x for x in (msb, lsb, expires_ms))) + pub_der,
                       padding.PKCS1v15(), hashes.SHA1())
    pem_body = lambda der: "\n".join(base64.b64encode(der).decode()[i:i + 76] for i in range(0, len(base64.b64encode(der)), 76))
    priv_der = key.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())
    return {
        "keyPair": {
            "privateKey": f"-----BEGIN RSA PRIVATE KEY-----\n{pem_body(priv_der)}\n-----END RSA PRIVATE KEY-----\n",
            "publicKey": f"-----BEGIN RSA PUBLIC KEY-----\n{pem_body(pub_der)}\n-----END RSA PUBLIC KEY-----\n",
        },
        # 1.19.0's v1 signature is not produced: that release is out of scope
        "publicKeySignature": base64.b64encode(v2).decode(),
        "publicKeySignatureV2": base64.b64encode(v2).decode(),
        "expiresAt": expires.isoformat().replace("+00:00", "Z"),
        "refreshedAfter": (now + datetime.timedelta(hours=40)).isoformat().replace("+00:00", "Z"),
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("yggdrasil " + fmt % args + "\n")

    def reply(self, code, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(code)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def token_name(self):
        auth = self.headers.get("Authorization", "")
        token = auth.removeprefix("Bearer ").strip()
        return token.removeprefix("tok-") if token.startswith("tok-") else None

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path in ("", "/"):
            return self.reply(200, {
                "meta": {"serverName": "presence-mock", "implementationName": "presence-compat", "implementationVersion": "1",
                         "feature.non_email_login": True, "feature.enable_profile_key": True},
                "skinDomains": [],
                "signaturePublickey": ROOT_PUB_PEM,
            })
        if u.path == "/sessionserver/session/minecraft/hasJoined":
            name, server_id = q.get("username", [""])[0], q.get("serverId", [""])[0]
            if joins.get(server_id) == name:
                return self.reply(200, profile(name))
            return self.reply(204)
        if u.path.startswith("/sessionserver/session/minecraft/profile/"):
            pid = u.path.rsplit("/", 1)[1].replace("-", "")
            for name in set(joins.values()):
                if offline_uuid(name).hex == pid:
                    return self.reply(200, profile(name))
            return self.reply(204)
        if u.path == "/minecraftservices/publickeys":
            key = {"publicKey": base64.b64encode(ROOT_PUB_DER).decode()}
            return self.reply(200, {"profilePropertyKeys": [key], "playerCertificateKeys": [key], "authenticationKeys": [key]})
        self.reply(404, {"error": "NotFound", "path": u.path})

    def do_POST(self):
        u = urlparse(self.path)
        if u.path == "/sessionserver/session/minecraft/join":
            b = self.body()
            token, selected, server_id = b.get("accessToken", ""), b.get("selectedProfile", ""), b.get("serverId", "")
            name = token.removeprefix("tok-") if token.startswith("tok-") else None
            if not name or offline_uuid(name).hex != selected.replace("-", ""):
                return self.reply(403, {"error": "ForbiddenOperationException", "errorMessage": "Invalid token."})
            joins[server_id] = name
            return self.reply(204)
        if u.path == "/api/profiles/minecraft":
            return self.reply(200, [profile(n) for n in self.body()])
        if u.path == "/minecraftservices/player/certificates":
            name = self.token_name()
            if not name:
                return self.reply(401, {"error": "Unauthorized"})
            return self.reply(200, certificate(name))
        self.reply(404, {"error": "NotFound", "path": u.path})


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
