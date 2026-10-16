//! The shared event wait.

use std::time::Duration;

use tokio::sync::broadcast;

use crate::BotEvent;

/// Receive until `pred` matches, `timeout` elapses, or the stream dies —
/// the shared event wait (upstream's `once` with a cap so a dropped packet
/// can't hang the caller). `None` on timeout, on `End`, or on a closed
/// stream; the caller decides which of those it can treat alike.
pub(crate) async fn await_event(
    rx: &mut broadcast::Receiver<BotEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&BotEvent) -> bool,
) -> Option<BotEvent> {
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return Some(event),
                Ok(BotEvent::End(_)) => return None,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_default()
}
