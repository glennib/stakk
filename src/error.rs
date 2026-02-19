use miette::Diagnostic;
use thiserror::Error;

use crate::auth::AuthError;
use crate::forge::ForgeError;
use crate::jj::JjError;

/// Errors that can occur in jack.
#[derive(Debug, Error, Diagnostic)]
pub enum JackError {
    /// An error from interacting with the jj CLI.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Jj(#[from] JjError),

    /// An error from the forge (e.g. GitHub API).
    #[error(transparent)]
    #[diagnostic(transparent)]
    Forge(#[from] ForgeError),

    /// An authentication error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Auth(#[from] AuthError),

    /// An error in change graph construction.
    #[error("graph error: {message}")]
    Graph { message: String },
}
