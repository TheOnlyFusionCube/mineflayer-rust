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
    /// A bot operation failed with an upstream-style message (e.g.
    /// `already sleeping`, `Fishing cancelled`) — the Rust analog of
    /// mineflayer's `new Error(message)` rejections.
    #[error("{0}")]
    Operation(String),
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
}
