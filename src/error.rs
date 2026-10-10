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
    /// The operation requires a connected bot.
    #[error("not connected")]
    NotConnected,
    /// An operation did not complete in time.
    #[error("timed out")]
    Timeout,
}

/// Convenience alias for `Result<T, mineflayer::Error>`.
pub type Result<T> = std::result::Result<T, Error>;
