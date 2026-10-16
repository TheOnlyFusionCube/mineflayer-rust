//! Error type for the `mineflayer` facade.

/// Errors surfaced by `mineflayer` operations.
///
/// Later units add one variant per domain as those methods land; azalea error
/// types are wrapped rather than re-exported so the facade's `Error` stays
/// stable when azalea's internals shift.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The server address could not be resolved or the connection failed.
    /// Carries the message so no third-party error type is in this API.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The bot's runtime (thread/local task set) could not start, or
    /// exited during connect.
    #[error("bot runtime failed: {0}")]
    Runtime(String),
    /// Microsoft authentication failed (device-code flow or token refresh).
    /// Wraps [`azalea::auth::AuthError`].
    #[error("authentication failed: {0}")]
    Auth(#[from] azalea::auth::AuthError),
    /// The operation requires a connected bot.
    #[error("not connected")]
    NotConnected,
    /// An operation did not complete in time.
    #[error("timed out")]
    Timeout,
    /// An argument is outside its valid range or names the wrong kind of
    /// target (bad slot, bad mouse mode, a block that is not a bed, a
    /// window that is not a furnace). Change the call to correct it.
    ///
    /// The message is the upstream `new Error(message)` text where upstream
    /// has one. Each message-carrying variant below keeps that text as its
    /// `Display` output, so code ported from mineflayer that compares
    /// messages still works.
    #[error("{0}")]
    InvalidArgument(String),
    /// The bot is not in a state that permits the operation now (already
    /// sleeping, empty hand, full food bar, a window is open, not in
    /// creative mode). The same call can succeed later.
    #[error("{0}")]
    InvalidState(String),
    /// A required item, block, entity, chest, recipe ingredient, or trade
    /// is not available.
    #[error("{0}")]
    NotFound(String),
    /// The server refused the action, did not answer, or the action did not
    /// complete in its time limit (`Server rejected`, `path to … timed
    /// out`, `Eating timed out …`).
    #[error("{0}")]
    Failed(String),
    /// A newer call to the same operation, or an explicit cancel, ended
    /// this one (`… cancelled due to calling bot.fish() again`).
    #[error("{0}")]
    Cancelled(String),
    /// The operation or option is not supported by this port.
    #[error("{0}")]
    Unsupported(String),
    /// A [`crate::Bot::dig`] ended without breaking the block. The
    /// message is upstream's `Digging aborted`; the payload names the cause.
    #[error("Digging aborted")]
    DigAborted(crate::interact::DigAbortReason),
    /// Sign text failed validation (upstream emits the same as an error
    /// event from `updateSign` before writing).
    #[error("invalid sign text: {0}")]
    SignText(String),
    /// A `pathfinder.goto` rejection — upstream puts these names on
    /// `err.name` (`NoPath`, `Timeout`, `GoalChanged`, `PathStopped`) so
    /// callers can discriminate without parsing the message.
    #[error("{0}")]
    Path(#[from] crate::pathfinder::PathError),
}

/// Convenience alias for `Result<T, mineflayer::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// `Connect` carries a message, so no third-party (hickory) type is in
    /// the public API; thread-start failures are their own variant.
    #[test]
    fn connect_and_runtime_hold_strings() {
        let connect: String = match Error::Connect("bad address".to_string()) {
            Error::Connect(message) => message,
            other => panic!("expected Connect, got {other:?}"),
        };
        assert_eq!(connect, "bad address");
        assert_eq!(
            Error::Connect("bad address".to_string()).to_string(),
            "connect failed: bad address"
        );
        assert_eq!(
            Error::Runtime("could not spawn the bot thread".to_string()).to_string(),
            "bot runtime failed: could not spawn the bot thread"
        );
    }

    /// The typed variants that replaced `Operation(String)` keep the exact
    /// upstream message as `Display`. Eight samples come through the real
    /// code paths; the last two pin the variants that have no cheap path.
    #[tokio::test]
    async fn typed_errors_keep_upstream_messages() {
        use crate::{EquipDest, Item, interact::DigAbortReason, inventory::click_operation};

        let bot = crate::Bot::new(crate::test_util::despawned_client());
        let empty = Item {
            stack: azalea::inventory::ItemStack::Empty,
            slot: 0,
        };
        let cases: Vec<(Error, &str)> = vec![
            (
                click_operation(0, 5, 0).unwrap_err(),
                "invalid mouse button 5",
            ),
            (
                click_operation(0, 0, 5).unwrap_err(),
                "unsupported mouse mode 5",
            ),
            (click_operation(0, 0, 9).unwrap_err(), "invalid mode 9"),
            (
                bot.set_quick_bar_slot(9).await.unwrap_err(),
                "quick_bar_slot must be 0..=8, got 9",
            ),
            (
                bot.chat("hi").unwrap_err(),
                "bot.chat() called before the client entered the play state; \
                 wait for the \"login\" or \"spawn\" event",
            ),
            (bot.wake().unwrap_err(), "already awake"),
            (
                bot.creative()
                    .set_inventory_slot(45, azalea::inventory::ItemStack::Empty)
                    .await
                    .unwrap_err(),
                "slot must be <= 44",
            ),
            (
                bot.equip(&empty, EquipDest::Hand).await.unwrap_err(),
                "Invalid item object in equip",
            ),
            (
                Error::DigAborted(DigAbortReason::Stopped),
                "Digging aborted",
            ),
            (
                Error::Cancelled("Fishing cancelled".into()),
                "Fishing cancelled",
            ),
        ];
        for (err, text) in &cases {
            assert_eq!(err.to_string(), *text, "{err:?}");
        }
        assert!(matches!(cases[0].0, Error::InvalidArgument(_)));
        assert!(matches!(cases[1].0, Error::Unsupported(_)));
        assert!(matches!(cases[4].0, Error::InvalidState(_)));
        assert!(matches!(cases[6].0, Error::InvalidArgument(_)));
    }
}
