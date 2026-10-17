//! The shared event wait.

use std::time::Duration;

use tokio::sync::broadcast;

use crate::{BotEvent, Error};

/// Why an event wait ended without a match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitEnd {
    /// The time limit elapsed.
    Timeout,
    /// The bot emitted `End`: the connection is over.
    Ended,
    /// The event stream closed.
    Closed,
}

impl From<WaitEnd> for Error {
    fn from(end: WaitEnd) -> Self {
        match end {
            WaitEnd::Timeout => Error::Timeout,
            WaitEnd::Ended | WaitEnd::Closed => Error::NotConnected,
        }
    }
}

/// Receive until `pred` matches, `timeout` elapses, or the stream dies —
/// the shared event wait (upstream's `once` with a cap so a dropped packet
/// can't hang the caller). `pred` also sees the `End` event before the
/// wait ends on it.
pub(crate) async fn await_event(
    rx: &mut broadcast::Receiver<BotEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&BotEvent) -> bool,
) -> Result<BotEvent, WaitEnd> {
    await_map(rx, timeout, |event| pred(event).then(|| event.clone())).await
}

/// [`await_event`] for a wait that computes its result from the matching
/// event: the wait ends on the first event `f` maps to `Some`.
pub(crate) async fn await_map<T>(
    rx: &mut broadcast::Receiver<BotEvent>,
    timeout: Duration,
    mut f: impl FnMut(&BotEvent) -> Option<T>,
) -> Result<T, WaitEnd> {
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(found) = f(&event) {
                        return Ok(found);
                    }
                    if matches!(event, BotEvent::End(_)) {
                        return Err(WaitEnd::Ended);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Err(WaitEnd::Closed),
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or(Err(WaitEnd::Timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_wait_reports_why_it_ended() {
        let (tx, mut rx) = broadcast::channel(8);
        tx.send(BotEvent::Tick).unwrap();
        tx.send(BotEvent::Health).unwrap();
        let found = await_event(&mut rx, Duration::from_secs(1), |e| {
            matches!(e, BotEvent::Health)
        })
        .await;
        assert!(matches!(found, Ok(BotEvent::Health)));

        tx.send(BotEvent::End("x".into())).unwrap();
        let ended = await_event(&mut rx, Duration::from_secs(1), |_| false).await;
        assert_eq!(ended.unwrap_err(), WaitEnd::Ended);

        let timed_out = await_event(&mut rx, Duration::from_millis(10), |_| false).await;
        assert_eq!(timed_out.unwrap_err(), WaitEnd::Timeout);

        drop(tx);
        let closed = await_event(&mut rx, Duration::from_secs(1), |_| false).await;
        assert_eq!(closed.unwrap_err(), WaitEnd::Closed);
    }
}
