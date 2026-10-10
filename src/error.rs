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
    /// Wraps the error returned by [`azalea::Client::join`].
    #[error("connect failed: {0}")]
    Connect(#[from] azalea::protocol::resolve::ResolveError),
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
}

/// Convenience alias for `Result<T, mineflayer::Error>`.
pub type Result<T> = std::result::Result<T, Error>;
