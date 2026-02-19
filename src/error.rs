use thiserror::Error;

use crate::jj::JjError;

/// Errors that can occur in jack.
#[expect(
    dead_code,
    reason = "variants will be used as milestones are implemented"
)]
#[derive(Debug, Error)]
pub enum JackError {
    /// An error from interacting with the jj CLI.
    #[error(transparent)]
    Jj(#[from] JjError),

    /// An error from the forge (e.g. GitHub API).
    #[error("forge error: {message}")]
    Forge { message: String },

    /// An authentication error.
    #[error("auth error: {message}")]
    Auth { message: String },

    /// An error in change graph construction.
    #[error("graph error: {message}")]
    Graph { message: String },
}
