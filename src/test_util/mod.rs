//! Test harnesses, for this crate and, through the `test-util` feature,
//! for plugin crates. [`sim::BotSim`] runs the real engine and bridge
//! with no server; the scaffolds are clients and bots with no engine
//! systems.

#[cfg(test)]
mod golden;
mod scaffold;
pub mod sim;

pub use scaffold::*;
