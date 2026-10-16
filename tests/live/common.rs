//! Helpers shared by the live tests.

#![cfg(feature = "live")]

use std::{collections::VecDeque, fmt::Write as _, time::Duration};

use mineflayer::BotEvent;
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// How many recent events a timeout message lists (E5 flake diagnostics).
/// `Packet` and `Tick` are left out: they would fill the list.
const RECENT: usize = 20;

fn worth_listing(event: &BotEvent) -> bool {
    !matches!(event, BotEvent::Packet(_) | BotEvent::Tick)
}

/// Keeps the `Debug` text up to the first non-identifier char, then stops
/// the formatter — a variant name without formatting a whole packet.
struct VariantName(String);

impl std::fmt::Write for VariantName {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            if c.is_alphanumeric() || c == '_' {
                self.0.push(c);
            } else {
                return Err(std::fmt::Error);
            }
        }
        Ok(())
    }
}

/// The `BotEvent` variant name (`"Packet"`, `"End"`, …).
pub(crate) fn variant_name(event: &BotEvent) -> String {
    let mut name = VariantName(String::new());
    let _ = write!(name, "{event:?}");
    name.0
}

/// Variant names of the events still queued on `rx` (the last
/// [`RECENT`]), without waiting — for a failure message.
pub(crate) fn drain_recent(rx: &mut Receiver<BotEvent>) -> Vec<String> {
    let mut recent = VecDeque::new();
    loop {
        match rx.try_recv() {
            Ok(event) if worth_listing(&event) => {
                if recent.len() == RECENT {
                    recent.pop_front();
                }
                recent.push_back(variant_name(&event));
            }
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return recent.into(),
        }
    }
}

/// Receive events until one satisfies `pred` (`Lagged` just re-reads).
/// Panics on timeout or a closed stream; the timeout message gives the
/// elapsed time and the last [`RECENT`] event variants seen.
pub(crate) async fn wait_for(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> BotEvent {
    let start = std::time::Instant::now();
    let mut recent: VecDeque<String> = VecDeque::new();
    let found = tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(event) if worth_listing(&event) => {
                    if recent.len() == RECENT {
                        recent.pop_front();
                    }
                    recent.push_back(variant_name(&event));
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => recent.push_back(format!("<lagged {n}>")),
                Err(RecvError::Closed) => panic!("event stream closed while waiting for {what}"),
            }
        }
    })
    .await;
    found.unwrap_or_else(|_| {
        panic!(
            "timed out after {:?} waiting for {what}; last events: {:?}",
            start.elapsed(),
            recent
        )
    })
}

/// Drain events until `pred` matches, returning every event consumed —
/// the caller can assert on the whole stream (e.g. ForcedMove arriving on
/// the way to Spawn), not just the matched event.
pub(crate) async fn collect_until(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> Vec<BotEvent> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                let done = pred(&event);
                seen.push(event);
                if done {
                    return seen;
                }
            }
            Ok(Err(RecvError::Lagged(_))) => {}
            Ok(Err(RecvError::Closed)) => {
                panic!("event stream closed while waiting for {what}")
            }
            Err(_) => panic!("timed out after {timeout:?} waiting for {what}"),
        }
    }
}

/// Horizontal (xz) distance — the plane pathfinder/movement paths follow.
pub(crate) fn horizontal_distance(a: azalea::Vec3, b: azalea::Vec3) -> f64 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    (dx * dx + dz * dz).sqrt()
}
