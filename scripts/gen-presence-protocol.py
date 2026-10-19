#!/usr/bin/env python3
"""Write crates/mineflayer-presence/protocol.json, the packet table of the
presence client, from a minecraft-data checkout (PrismarineJS).

Usage: python3 scripts/gen-presence-protocol.py <minecraft-data dir>

Releases newer than minecraft-data are derived (DERIVED below) from the
server's packet report in scripts/presence-reports and the layout changes
ViaVersion's rewriters list for them.

The table has one row per protocol number from 1.8 (47) up. A row maps each
packet of the presence subset (SUBSET below) to its packet id and the index
of its schema in "schemas". A schema is the minecraft-data protodef type with
every named type in a type position inlined, except the types minecraft-data marks "native",
which stay as their name and which the interpreter implements. Identical
schemas are stored once.
"""
import json
import re
import subprocess
import sys

MIN_PROTOCOL = 4

SUBSET = {
    "login.toServer": ["login_start", "encryption_begin", "login_plugin_response", "login_acknowledged", "cookie_response"],
    "login.toClient": ["disconnect", "encryption_begin", "success", "compress", "login_plugin_request", "cookie_request"],
    "configuration.toServer": ["custom_payload", "settings", "finish_configuration", "keep_alive", "pong", "select_known_packs", "cookie_response", "resource_pack_receive", "accept_code_of_conduct"],
    "configuration.toClient": ["transfer", "store_cookie", "disconnect", "finish_configuration", "keep_alive", "ping", "select_known_packs", "cookie_request", "add_resource_pack", "resource_pack_send", "code_of_conduct"],
    "play.toServer": ["custom_payload", "look", "entity_action", "player_input", "chat_session_update", "chat_command_signed", "keep_alive", "pong", "teleport_confirm", "chunk_batch_received", "configuration_acknowledged", "client_command", "chat", "chat_message", "chat_command", "message_acknowledgement", "arm_animation", "player_loaded", "settings", "resource_pack_receive", "position_look", "cookie_response"],
    "play.toClient": ["login", "respawn", "transfer", "store_cookie", "keep_alive", "ping", "kick_disconnect", "position", "chunk_batch_finished", "start_configuration", "update_health", "chat", "system_chat", "player_chat", "profileless_chat", "resource_pack_send", "add_resource_pack", "cookie_request", "set_compression"],
}


# Packets the session recognizes by id only: it reads the entity id prefix of
# login and never decodes respawn. Their schema is stored as restBuffer, and
# their layouts do not decide aliasing.
BY_ID_ONLY = {("play.toClient", "login"), ("play.toClient", "respawn")}


# Releases minecraft-data has no data for yet. Ids come from the server's
# packet report (scripts/presence-reports/<name>.json), matched to presence
# packets by Mojang name through the 26.1 report. `schemas` replaces a layout
# that changed, `added` names new packets, `removed` drops packets the
# release no longer has. Each change cites ViaVersion b712c00, which
# translates between consecutive releases.
DERIVED = [
    {
        "protocol": 776,
        "names": ["26.2"],
        "base": 775,
        "schemas": {
            # Protocol26_1To26_2.java:105, LOGIN_FINISHED gains a session id
            ("login.toClient", "success"): ["container", [
                {"name": "uuid", "type": "UUID"},
                {"name": "username", "type": ["pstring", {"countType": "varint"}]},
                {"name": "properties", "type": "$properties"},
                {"name": "sessionId", "type": "UUID"},
            ]],
        },
    },
    {
        "protocol": 777,
        "names": ["26.3"],
        "base": 776,
        "schemas": {
            # EntityPacketRewriter26_3.java:126, ACCEPT_TELEPORTATION carries
            # the position the client accepted
            ("play.toServer", "teleport_confirm"): ["container", [
                {"name": "teleportId", "type": "varint"},
                {"name": "x", "type": "f64"},
                {"name": "y", "type": "f64"},
                {"name": "z", "type": "f64"},
                {"name": "yaw", "type": "f32"},
                {"name": "pitch", "type": "f32"},
            ]],
        },
        # EntityPacketRewriter26_3.java:147, SWING is replaced by an empty
        # PUNCH, which Via turns back into a main-hand swing
        "added": {("play.toServer", "punch"): ("minecraft:punch", ["container", []])},
        "removed": [("play.toServer", "arm_animation")],
    },
]

# Releases that share a protocol number with a row but are not in
# minecraft-data's dataPaths (read from each server jar's version.json).
EXTRA_NAMES = {5: ["1.7.6", "1.7.7", "1.7.8", "1.7.9", "1.7.10"], 775: ["26.1.1", "26.1.2"]}

# 1.7.2 to 1.7.5 (protocol 4) have no minecraft-data. 1.7.6 (protocol 5)
# changed only the UUID text of login success (now hyphenated) and the player
# properties of spawn_player; neither changes a presence packet's layout
# (login success's uuid stays a string). Checked live on a 1.7.2 server.
OLDER_SAME_AS = {4: (5, ["1.7.2", "1.7.4", "1.7.5"])}

REPORT_STATE = {"login": "login", "configuration": "configuration", "play": "play"}
REPORT_DIR = {"toServer": "serverbound", "toClient": "clientbound"}


def derive(rows, schemas, index, reports_dir):
    def report(name):
        return json.load(open(f"{reports_dir}/{name}.json"))

    bridge = report("26.1")
    bridge_row = rows[775]["packets"]
    by_id = {
        key: {pid: name for name, pid in bridge[REPORT_STATE[key.split(".")[0]]][REPORT_DIR[key.split(".")[1]]].items()}
        for key in bridge_row
    }
    mojang = {(key, n): by_id[key][e[0]] for key, table in bridge_row.items() for n, e in table.items()}

    def intern(schema):
        text = json.dumps(schema, sort_keys=True, separators=(",", ":"))
        if text not in index:
            index[text] = len(schemas)
            schemas.append(json.loads(text))
        return index[text]

    for spec in DERIVED:
        ids = report(spec["names"][0])
        base = rows[spec["base"]]["packets"]
        packets = {}
        for key, table in base.items():
            st, d = key.split(".")
            live = ids[REPORT_STATE[st]][REPORT_DIR[d]]
            out = {}
            for n, e in table.items():
                if (key, n) in spec.get("removed", []):
                    continue
                name = mojang[(key, n)]
                if name not in live:
                    sys.exit(f"{spec['names'][0]}: {name} ({key} {n}) is gone; list it in removed")
                schema = e[1]
                if (key, n) in spec.get("schemas", {}):
                    schema = intern(resolve(spec["schemas"][(key, n)], schemas[e[1]]))
                out[n] = [live[name], schema]
            for (k2, n2), (name, schema) in spec.get("added", {}).items():
                if k2 == key:
                    mojang[(k2, n2)] = name
                    out[n2] = [live[name], intern(schema)]
            packets[key] = out
        rows[spec["protocol"]] = {"protocol": spec["protocol"], "names": list(spec["names"]), "packets": packets}


def resolve(schema, base):
    """Fill "$<field>" types in a derived schema from the same field of the
    base layout, so an override restates only what changed."""
    base_fields = {f["name"]: f["type"] for f in base[1]} if isinstance(base, list) and base[0] == "container" else {}

    def walk(t):
        if isinstance(t, str) and t.startswith("$"):
            return base_fields[t[1:]]
        if isinstance(t, list):
            return [walk(x) for x in t]
        if isinstance(t, dict):
            return {k: walk(v) for k, v in t.items()}
        return t

    return walk(schema)


def alias_key(packets):
    return {
        key: {n: (e[0] if (key, n) in BY_ID_ONLY else e) for n, e in table.items()}
        for key, table in packets.items()
    }


def inline(t, types, stack=()):
    if isinstance(t, str):
        d = types.get(t)
        if d is None or d == "native" or t in stack:
            return t
        return inline(d, types, stack + (t,))
    if isinstance(t, list):
        return [inline(x, types, stack) for x in t]
    if isinstance(t, dict):
        out = {}
        for k, v in t.items():
            if k in ("type", "default"):
                out[k] = inline(v, types, stack)
            elif k == "otherwise":
                out[k] = dict(v, type=inline(v["type"], types, stack))
            elif k == "fields":
                out[k] = {fk: inline(fv, types, stack) for fk, fv in v.items()}
            else:
                out[k] = v
        return out
    return t


def main(md):
    data_paths = json.load(open(f"{md}/data/dataPaths.json"))["pc"]
    commit = subprocess.run(["git", "-C", md, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    rows, schemas, index = {}, [], {}
    for name, paths in data_paths.items():
        if "protocol" not in paths or not re.fullmatch(r"\d+\.\d+(\.\d+)?", name):
            continue
        protocol = json.load(open(f"{md}/data/{paths['version']}/version.json"))["version"]
        if protocol < MIN_PROTOCOL:
            continue
        if protocol in rows:
            rows[protocol]["names"].append(name)
            continue
        pj = json.load(open(f"{md}/data/{paths['protocol']}/protocol.json"))
        packets = {}
        for key, wanted in SUBSET.items():
            state, direction = key.split(".")
            st = pj.get(state, {}).get(direction, {}).get("types", {})
            if "packet" not in st:
                continue
            types = dict(pj.get("types", {}), **st)
            mappings = st["packet"][1][0]["type"][1]["mappings"]
            ids = {n: int(h, 16) for h, n in mappings.items()}
            fields = st["packet"][1][1]["type"][1]["fields"]
            table = {}
            for n in wanted:
                if n not in ids:
                    continue
                body = "restBuffer" if (key, n) in BY_ID_ONLY else inline(fields[n], types)
                schema = json.dumps(body, sort_keys=True, separators=(",", ":"))
                if schema not in index:
                    index[schema] = len(schemas)
                    schemas.append(json.loads(schema))
                table[n] = [ids[n], index[schema]]
            packets[key] = table
        rows[protocol] = {"protocol": protocol, "names": [name], "packets": packets}
    derive(rows, schemas, index, "scripts/presence-reports")
    for p, (same, names) in OLDER_SAME_AS.items():
        rows[p] = {"protocol": p, "names": names, "packets": rows[same]["packets"]}
    # A release minecraft-data has no data for (1.9.1, 1.14.2) gets the row of
    # its neighbours when the protocols on both sides agree on every packet.
    for v in json.load(open(f"{md}/data/pc/common/protocolVersions.json")):
        p, name = v["version"], v["minecraftVersion"]
        if not v.get("usesNetty") or p < MIN_PROTOCOL or p in rows or not re.fullmatch(r"\d+\.\d+(\.\d+)?", name):
            continue
        lower = [q for q in rows if q < p]
        upper = [q for q in rows if q > p]
        if lower and upper and alias_key(rows[max(lower)]["packets"]) == alias_key(rows[min(upper)]["packets"]):
            rows.setdefault(p, {"protocol": p, "names": [], "packets": rows[max(lower)]["packets"]})["names"].append(name)
        elif p not in rows:
            print(f"no data for {name} (protocol {p})", file=sys.stderr)
    for p, names in EXTRA_NAMES.items():
        rows[p]["names"].extend(names)

    out = {
        "source": f"PrismarineJS/minecraft-data@{commit}",
        "versions": [rows[p] for p in sorted(rows)],
        "schemas": schemas,
    }
    with open("crates/mineflayer-presence/protocol.json", "w") as f:
        json.dump(out, f, separators=(",", ":"))
        f.write("\n")
    print(f"{len(rows)} protocols, {len(schemas)} schemas")


if __name__ == "__main__":
    main(sys.argv[1])
