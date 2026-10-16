//! Helpers shared by the live tests.

#![cfg(feature = "live")]

use std::time::Duration;

use mineflayer::BotEvent;
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Receive events until one satisfies `pred` (`Lagged` just re-reads).
/// Panics on timeout or a closed stream.
pub(crate) async fn wait_for(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> BotEvent {
    tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("event stream closed while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
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
