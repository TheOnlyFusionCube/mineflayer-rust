"""Copy the upstream engine (ref/azalea at ddef371) into crates/ under mineflayer-* names.

Step one of the engine move in commit ed84001, kept so an upstream merge can
repeat the token renames (docs/decisions.md, D-engine). Usage:
    python3 -I scripts/vendor_engine.py "$PWD"    # refuses if crates/ exists
After this script, the move also ran: a prose pass over comments and test
strings (listed in D-engine), `cargo fmt --all` (crates/rustfmt.toml), and
`cargo clippy --fix`. Those passes reformatted upstream code; diff against
this script's raw output, not against upstream, when merging.
"""
import pathlib
import re
import shutil
import sys

REPO = pathlib.Path(sys.argv[1])
SRC = REPO / "ref" / "azalea"
DST = REPO / "crates"

CRATES = {
    "azalea": "engine",
    "azalea-auth": "auth",
    "azalea-block": "block",
    "azalea-block/azalea-block-macros": "block-macros",
    "azalea-brigadier": "brigadier",
    "azalea-buf": "buf",
    "azalea-buf/azalea-buf-macros": "buf-macros",
    "azalea-chat": "chat",
    "azalea-client": "client",
    "azalea-core": "core",
    "azalea-crypto": "crypto",
    "azalea-entity": "entity",
    "azalea-inventory": "inventory",
    "azalea-inventory/azalea-inventory-macros": "inventory-macros",
    "azalea-language": "language",
    "azalea-physics": "physics",
    "azalea-protocol": "protocol",
    "azalea-protocol/azalea-protocol-macros": "protocol-macros",
    "azalea-registry": "registry",
    "azalea-registry/azalea-registry-macros": "registry-macros",
    "azalea-world": "world",
}
KEEP = {"src", "tests", "build.rs", "Cargo.toml"}

SUFFIXES = sorted({v for v in CRATES.values() if v != "engine"}, key=len, reverse=True)
HY = "|".join(SUFFIXES)
US = "|".join(s.replace("-", "_") for s in SUFFIXES)

RULES = [
    (rf"\bazalea-({HY})\b", r"mineflayer-\1"),
    (rf"\bazalea_({US})\b", r"mineflayer_\1"),
    (r"\bcreate_fn_azalea_(read|write)\b", r"create_fn_mc_\1"),
    (r"\bazalea_(read_var|write_var|read_limited|read_as|write_as|read|write)\b", r"mc_\1"),
    (r"\bAzBuf(Var|Limited)?\b", r"McBuf\1"),
    (r"\bAZALEA_DO_NOT_CUT_OFF_PACKET_LOGS\b", "MINEFLAYER_DO_NOT_CUT_OFF_PACKET_LOGS"),
    (r"\bAzaleaPlugin\b", "EnginePlugin"),
    (r"\bazalea::", "mineflayer_engine::"),
    (r"\buse azalea\b", "use mineflayer_engine"),
]


def rename(text: str) -> str:
    for pat, rep in RULES:
        text = re.sub(pat, rep, text)
    return text


def manifest(text: str, new_name: str) -> str:
    text = re.sub(r'(?m)^name = "azalea"$', f'name = "{new_name}"', text)
    text = re.sub(r"(?m)^repository\.workspace = true\n", "", text)
    text = re.sub(r"(?ms)^\[package\.metadata\.release\]\n.*?\n\]\n\n?", "", text)
    text = re.sub(r"(?ms)^\[\[(bench|example)\]\]\n(?:(?!^\[).*\n?)*", "", text)
    text = re.sub(r"(?m)^criterion(\.workspace = true| = .*)\n", "", text)
    return rename(text)


def main() -> None:
    if DST.exists():
        sys.exit(f"{DST} exists")
    for old, suffix in CRATES.items():
        src, new = SRC / old, f"mineflayer-{suffix}"
        out = DST / new
        out.mkdir(parents=True)
        for entry in src.iterdir():
            if entry.name not in KEEP:
                continue
            if entry.is_dir():
                shutil.copytree(entry, out / entry.name)
            else:
                shutil.copy2(entry, out / entry.name)
        (out / "README.md").write_text(f"# {new}\n\nPart of the mineflayer engine.\n")
        for path in out.rglob("*"):
            if path.is_file() and path.suffix in {".rs", ".toml", ".md", ".json"} and path.name != "en_us.json":
                body = path.read_text()
                body = manifest(body, new) if path.name == "Cargo.toml" else rename(body)
                path.write_text(body)
    shutil.copy2(SRC / "rustfmt.toml", DST / "rustfmt.toml")


main()
