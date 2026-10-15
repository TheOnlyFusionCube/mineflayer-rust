#!/usr/bin/env python3
"""Focused checks for argon-unit.py's write-boundary validation.

Run: python3 scripts/test-argon-unit.py
Covers: ../ traversal, paths outside the brief's Writable list, valid
allowed patches, mismatched patch headers, and extra-file patches.
"""
import importlib.util
import os
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location(
    "argon_unit", os.path.join(ROOT, "scripts", "argon-unit.py")
)
module = importlib.util.module_from_spec(spec)
# execute the module minus main()
src = open(spec.origin).read().split("def main()")[0]
src = src.replace("__file__", repr(spec.origin))
ns = {}
exec(src, ns)

safe = ns["safe_repo_path"]
strict = ns["strict_patch"]
fails = []


def check(name, cond):
    print(("ok  " if cond else "FAIL") + " " + name)
    if not cond:
        fails.append(name)


check("traversal path rejected", safe("src/../scripts/verify.sh", ["src/../scripts/verify.sh"]) is None)
check("absolute path rejected", safe("/etc/passwd", ["/etc/passwd"]) is None)
check("path outside brief Writable rejected", safe("src/chat.rs", ["src/bot.rs"]) is None)
check("allowed path accepted", safe("src/chat.rs", ["src/chat.rs"]) == "src/chat.rs")

# an allowlisted in-root symlink pointing outside the Writable tree
with tempfile.TemporaryDirectory() as td:
    os.makedirs(os.path.join(td, "src"), exist_ok=True)
    open(os.path.join(td, "scripts_verify.sh"), "w").write("x")
    os.symlink(os.path.join(td, "scripts_verify.sh"), os.path.join(td, "src", "chat.rs"))
    real_root = ns["ROOT"]
    ns["ROOT"] = td
    try:
        check("symlinked path rejected", safe("src/chat.rs", ["src/chat.rs"]) is None)
    finally:
        ns["ROOT"] = real_root

patch_to_verify = "--- a/scripts/verify.sh\n+++ b/scripts/verify.sh\n@@ -1,1 +1,1 @@\n-x\n+y"
check("mismatched patch headers rejected", strict("src/chat.rs", patch_to_verify) is None)

two_files = (
    "diff --git a/src/chat.rs b/src/chat.rs\n--- a/src/chat.rs\n+++ b/src/chat.rs\n"
    "@@ -1,1 +1,1 @@\n-x\n+y\ndiff --git a/scripts/verify.sh b/scripts/verify.sh\n"
    "--- a/scripts/verify.sh\n+++ b/scripts/verify.sh\n@@ -1,1 +1,1 @@\n-x\n+y"
)
check("extra-file patch rejected", strict("src/chat.rs", two_files) is None)

out = strict("src/chat.rs", "@@ -1,1 +1,1 @@\n-x\n+y")
check("bare hunks normalized", out is not None and out.startswith("--- a/src/chat.rs\n+++ b/src/chat.rs\n"))

# a valid single-file patch actually applies in a temp repo
with tempfile.TemporaryDirectory() as td:
    subprocess.run(["git", "init", "-q"], cwd=td, check=True)
    os.makedirs(os.path.join(td, "src"), exist_ok=True)
    open(os.path.join(td, "src", "chat.rs"), "w").write("old\n")
    subprocess.run(["git", "add", "-A"], cwd=td, check=True)
    subprocess.run(["git", "commit", "-qm", "x"], cwd=td, check=True)
    p = os.path.join(td, "p.patch")
    open(p, "w").write(strict("src/chat.rs", "--- a/src/chat.rs\n+++ b/src/chat.rs\n@@ -1,1 +1,1 @@\n-old\n+new"))
    r = subprocess.run(["git", "apply", "--whitespace=nowarn", "--recount", p], cwd=td, capture_output=True)
    content = open(os.path.join(td, "src", "chat.rs")).read().strip()
    check("valid patch applies", r.returncode == 0 and content == "new")

if fails:
    print("FAILED:", fails)
    sys.exit(1)
print("PASS")
