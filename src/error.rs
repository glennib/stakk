use miette::Diagnostic;
use thiserror::Error;

use crate::auth::AuthError;
use crate::config::ConfigError;
use crate::forge::ForgeError;
use crate::forge::detect::DetectError;
use crate::jj::JjError;
use crate::select::bookmark_gen::BookmarkGenError;
use crate::submit::SubmitError;

/// Errors that can occur in stakk.
#[derive(Debug, Error, Diagnostic)]
pub enum StakkError {
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

    /// An error from the submission pipeline.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Submit(#[from] SubmitError),

    /// An error from the bookmark name generation command.
    #[error(transparent)]
    #[diagnostic(transparent)]
    BookmarkGen(#[from] BookmarkGenError),

    /// An error from the non-interactive selection flags.
    #[error(transparent)]
    #[diagnostic(transparent)]
    ExplicitSelection(#[from] crate::select::explicit::ExplicitSelectionError),

    /// A configuration error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] ConfigError),

    /// The remote's host could not be matched to a forge.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Detect(#[from] DetectError),

    /// The specified remote's URL has no `<host>/<owner>/<repo>` shape at all.
    #[error("remote '{name}' is not an owner/repo URL: {url}")]
    #[diagnostic(
        code(stakk::remote::not_a_repo_url),
        help(
            "stakk expects a <host>/<owner>/<repo> remote URL, e.g. git@github.com:owner/repo.git \
             or https://codeberg.org/owner/repo.git"
        )
    )]
    RemoteNotRepoUrl { name: String, url: String },

    /// The specified remote was not found.
    #[error("remote '{name}' not found")]
    #[diagnostic(
        code(stakk::remote::not_found),
        help("run `jj git remote list` to see available remotes")
    )]
    RemoteNotFound { name: String },

    /// Failed to load a custom template file.
    #[error("failed to load template '{path}': {reason}")]
    #[diagnostic(
        code(stakk::template::load_failed),
        help("check that the file exists and is readable")
    )]
    TemplateLoadFailed { path: String, reason: String },

    /// A CLI argument parsing error.
    #[error("{0}")]
    #[diagnostic(code(stakk::cli))]
    Cli(#[from] clap::Error),

    /// A terminal I/O error.
    #[error("terminal I/O error: {0}")]
    #[diagnostic(code(stakk::io))]
    Io(#[from] std::io::Error),

    /// Interactive selection required but stdin is not a terminal.
    #[error("interactive mode requires a terminal")]
    #[diagnostic(
        code(stakk::not_interactive),
        help(
            "select explicitly instead: stakk submit --keep <BOOKMARK> / --new <REV>[=<NAME>] — \
             run `stakk docs agents` for the full non-interactive workflow"
        )
    )]
    NotInteractive,

    /// User interrupted with Ctrl-C (exit 130).
    #[error("interrupted")]
    #[diagnostic(code(stakk::interrupted))]
    Interrupted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help(err: &StakkError) -> String {
        miette::Diagnostic::help(err).unwrap().to_string()
    }

    #[test]
    fn not_a_repo_url_is_forge_neutral() {
        let err = StakkError::RemoteNotRepoUrl {
            name: "origin".into(),
            url: "/srv/git/repo".into(),
        };
        assert_eq!(
            err.to_string(),
            "remote 'origin' is not an owner/repo URL: /srv/git/repo"
        );
        let help = help(&err);
        assert!(help.contains("<host>/<owner>/<repo>"), "{help}");
        assert!(!help.contains("--host"), "{help}");
        assert!(!help.contains("--forge"), "{help}");
    }
}
