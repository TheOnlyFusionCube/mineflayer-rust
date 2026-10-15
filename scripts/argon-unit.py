#!/usr/bin/env python3
"""argon-unit — run one refactor/audit unit on this repo via gemini-web2api (Argon).

Reads a markdown brief, sends it plus the listed context files to the local
Argon API, writes back the FILE blocks it returns (restricted to src/, docs/,
tests/), formats, then runs scripts/verify.sh.

Usage:
    python3 scripts/argon-unit.py --brief .live/argon/chat-audit.md [--timeout 900]

Brief format:
    # <unit name>
    ## Task
    <what to do, what counts as a real finding>
    ## Writable
    - src/chat.rs
    ## Context Files
    - ref/mineflayer/lib/plugins/chat.js
    - docs/api-parity-chat.md
    ## Extra Notes  (optional)

Output contract requested from the model:
    ===FILE: <path>===
    <full file contents>
    ===END===
    (repeat per changed file; unchanged files are omitted)
    ===NOTE===
    <short summary of findings and changes>
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATE = os.path.join(ROOT, ".live", "argon")
API = "http://localhost:8081/v1/chat/completions"
KEY = "sk-gemini"
MODEL = "gemini-4-argon@effort=high"
WRITABLE_RE = re.compile(r"^(src/|docs/|tests/)[A-Za-z0-9_./-]+$")

CONVENTIONS = """You are a senior Rust engineer working on mineflayer-rs: a Rust port of
the Node.js mineflayer Minecraft bot API built on azalea. The port keeps mineflayer's
names and semantics. Hard rules:
- Public API signatures must not change unless the task explicitly asks for it.
- No new dependencies. std, tokio, azalea, serde, thiserror, futures, regex, uuid only.
- No unsafe. Keep the existing code style and doc-comment style.
- Behavior must match the upstream JavaScript implementation listed in the context,
  except where docs/api-parity.md records an intentional divergence.
- Tests must keep passing (cargo fmt --check, cargo clippy -D warnings, cargo test, cargo build).

Response format, strict:
- For every file you change, emit either a full-file block:
  ===FILE: <repo-relative path>===
  <complete new contents of that file>
  ===END===
  or, preferrably for large files (> ~800 lines), a patch block:
  ===PATCH: <repo-relative path>===
  <unified diff: @@ -a,b +c,d @@ hunk headers and context lines copied
  exactly from the file — no markdown fences, no code fences>
  ===END PATCH===
- Do not emit files you did not change. Do not mix FILE and PATCH blocks for
  the same path. Do not use markdown fences anywhere.
- After all change blocks, emit:
  ===NOTE===
  <one short paragraph: findings and what changed, or why nothing needed to change>
"""


def log(msg):
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with open(os.path.join(STATE, "argon-unit.log"), "a") as f:
        f.write(line + "\n")


def parse_brief(path):
    text = open(path).read()
    files = []
    section = None
    for line in text.splitlines():
        if line.startswith("## "):
            section = line[3:].strip().lower()
            continue
        m = re.match(r"^-\s+([A-Za-z0-9_./-]+)\s*$", line.strip())
        if m and section == "context files":
            files.append(m.group(1))
    return text, files


def call_model(user_content, timeout):
    body = json.dumps({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": CONVENTIONS},
            {"role": "user", "content": user_content},
        ],
    }).encode()
    req = urllib.request.Request(API, data=body, headers={
        "Authorization": f"Bearer {KEY}",
        "Content-Type": "application/json",
    })
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.load(r)
    content = data["choices"][0]["message"]["content"]
    if not content or not content.strip():
        raise ValueError("empty model response")
    return content


def parse_blocks(response):
    files = {}
    # accept `===END===` and the `===END FILE: <path>===` variant models
    # mirror from the context framing
    for m in re.finditer(
        r"===FILE:\s*([A-Za-z0-9_./-]+)\s*===\n(.*?)\n===END(?: FILE:? *[A-Za-z0-9_./-]+)?===",
        response,
        re.S,
    ):
        files[m.group(1)] = m.group(2)
    patches = {}
    for m in re.finditer(
        r"===PATCH:\s*([A-Za-z0-9_./-]+)\s*===\n(.*?)\n===END PATCH===",
        response,
        re.S,
    ):
        patches[m.group(1)] = m.group(2)
    note = ""
    m = re.search(r"===NOTE===\s*(.*)$", response, re.S)
    if m:
        note = m.group(1).strip()
    return files, patches, note


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--brief", required=True)
    ap.add_argument("--timeout", type=int, default=900)
    ap.add_argument("--skip-verify", action="store_true")
    args = ap.parse_args()

    os.makedirs(STATE, exist_ok=True)
    unit = os.path.splitext(os.path.basename(args.brief))[0]
    brief_text, context_files = parse_brief(args.brief)

    parts = [brief_text, "\n## Context Files (verbatim)\n"]
    for rel in context_files:
        path = os.path.join(ROOT, rel)
        if not os.path.exists(path):
            log(f"context file missing: {rel}")
            sys.exit(2)
        content = open(path).read()
        parts.append(f"<<<BEGIN {rel}>>>\n{content}\n<<<END {rel}>>>\n")
    prompt = "\n".join(parts)
    log(f"[{unit}] sending {len(prompt)} chars, {len(context_files)} context files")

    t0 = time.time()
    response = call_model(prompt, args.timeout)
    raw_path = os.path.join(STATE, f"{unit}-raw.txt")
    with open(raw_path, "w") as f:
        f.write(response)
    log(f"[{unit}] model responded, {len(response)} chars in {time.time() - t0:.0f}s, raw -> {os.path.relpath(raw_path, ROOT)}")

    files, patches, note = parse_blocks(response)
    if note:
        log(f"[{unit}] note: {note[:400]}")
    if not files and not patches:
        log(f"[{unit}] no change blocks returned; nothing to apply")
        return

    for rel, diff in patches.items():
        if not WRITABLE_RE.match(rel) or not os.path.exists(os.path.join(ROOT, rel)):
            log(f"[{unit}] refusing path: {rel}")
            sys.exit(3)
        # models often emit bare hunks (`@@ ... @@`) — synthesize the
        # file headers `git apply` needs when they are missing
        diff_text = diff
        if not re.match(r"^\s*(diff --git|--- )", diff_text):
            diff_text = f"--- a/{rel}\n+++ b/{rel}\n{diff_text}"
        tmp = os.path.join(STATE, f"{unit}-{rel.replace('/', '_')}.patch")
        with open(tmp, "w") as f:
            f.write(diff_text + "\n")
        r = subprocess.run(
            ["git", "apply", "--whitespace=nowarn", "--recount", tmp],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if r.returncode != 0:
            log(f"[{unit}] patch for {rel} did NOT apply: {r.stderr.strip()[:300]}")
            sys.exit(3)
        log(f"[{unit}] patched {rel} ({len(diff)} chars)")

    for rel, content in files.items():
        if not WRITABLE_RE.match(rel) or not os.path.exists(os.path.join(ROOT, rel)):
            log(f"[{unit}] refusing path: {rel}")
            sys.exit(3)
        with open(os.path.join(ROOT, rel), "w") as f:
            f.write(content)
        log(f"[{unit}] wrote {rel} ({len(content)} chars)")

    subprocess.run(["cargo", "fmt"], cwd=ROOT, check=False)
    if args.skip_verify:
        log(f"[{unit}] verify skipped by flag; diff is staged in the working tree")
        return

    log(f"[{unit}] running scripts/verify.sh")
    r = subprocess.run(["bash", "scripts/verify.sh"], cwd=ROOT, capture_output=True, text=True)
    tail = (r.stdout + r.stderr).strip().splitlines()[-12:]
    with open(os.path.join(STATE, f"{unit}-verify.log"), "w") as f:
        f.write(r.stdout + "\n" + r.stderr)
    log(f"[{unit}] verify exit {r.returncode}: " + " | ".join(tail[-4:]))
    if r.returncode != 0:
        sys.exit(1)


if __name__ == "__main__":
    main()
