//! Test harnesses. [`sim::BotSim`] runs the real engine and bridge with
//! no server; plugin crates get it through the `test-util` feature. The
//! scaffolds (clients with no systems) are for this crate's unit tests.

#[cfg(test)]
mod golden;
#[cfg(test)]
mod scaffold;
pub mod sim;

#[cfg(test)]
pub(crate) use scaffold::*;
