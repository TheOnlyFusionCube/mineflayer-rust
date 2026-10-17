//! Every live test in one binary — each module holds one `#[ignore]`d test
//! against the local fixture (`scripts/live-server.sh`, port 25566):
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1
//! ```
//!
//! One binary (not twelve) keeps the engine link step and build disk use
//! in check. `--test-threads=1` is required: the tests share one fixture
//! world and some connect several bots.

#![cfg(feature = "live")]

mod common;

mod chat;
mod craft;
mod entity;
mod interact;
mod inventory;
mod lifecycle;
mod misc;
mod movement;
mod pathfinder;
mod plugins;
mod windows;
mod world;
