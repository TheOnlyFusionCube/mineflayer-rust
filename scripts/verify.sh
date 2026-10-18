#!/usr/bin/env bash
# verify.sh — per-unit verification gate for mineflayer-rs.
# Runs fmt, clippy, tests, and build via bare `cargo`, which resolves the
# toolchain pinned in rust-toolchain.toml (nightly-2026-10-01). Do NOT switch
# to `cargo +nightly` — that bypasses the pin and hits the rustc ICE
# documented there.
# Prints PASS/FAIL per step; exits nonzero if any step failed.
set -u
cd "$(dirname "$0")/.."

failures=0

run_step() {
    local name="$1"; shift
    echo "=== $name"
    if "$@"; then
        echo "--- $name: PASS"
    else
        echo "--- $name: FAIL"
        failures=$((failures + 1))
    fi
}

run_step "fmt"    cargo fmt --check
run_step "clippy" cargo clippy --all-targets -- -D warnings
# the live tests compile only with the feature; lint them too (their
# warnings otherwise stay invisible to this gate)
run_step "clippy (live)" cargo clippy --features live --all-targets -- -D warnings
run_step "clippy (engine)" env CARGO_TARGET_DIR=target/engine-log cargo clippy --workspace --exclude mineflayer --all-targets --features mineflayer-client/log -- -D warnings
run_step "test"   cargo test
# the engine crates, with the `log` feature their simulation tests capture
# logs through (off for the port, which never installs a subscriber). The
# two `log` steps build into target/engine-log, so either build directory
# can be removed without rebuilding the other.
run_step "test (engine)" env CARGO_TARGET_DIR=target/engine-log cargo test --workspace --exclude mineflayer --features mineflayer-client/log
run_step "build"  cargo build
run_step "doc"    env RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
# rustdoc takes RUSTDOCFLAGS, not the build rustflags, so it needs the
# solver pin itself (rust-toolchain.toml, .cargo/config.toml)
run_step "doc (engine)" env RUSTDOCFLAGS="-D warnings -Znext-solver=no" cargo doc --workspace --exclude mineflayer --no-deps

echo "==="
if [ "$failures" -eq 0 ]; then
    echo "verify: PASS"
    exit 0
else
    echo "verify: FAIL ($failures step(s) failed)"
    exit 1
fi
