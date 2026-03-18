use miette::Diagnostic;
use thiserror::Error;

use crate::auth::AuthError;
use crate::config::ConfigError;
use crate::forge::ForgeError;
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

    /// A configuration error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] ConfigError),

    /// The specified remote is not a GitHub URL.
    #[error("remote '{name}' is not a GitHub URL: {url}")]
    #[diagnostic(
        code(stakk::remote::not_github),
        help("stakk only supports GitHub remotes (github.com URLs)")
    )]
    RemoteNotGithub { name: String, url: String },

    /// The specified remote was not found.
    #[error("remote '{name}' not found")]
    #[diagnostic(
        code(stakk::remote::not_found),
        help("run `jj git remote list` to see available remotes")
    )]
    RemoteNotFound { name: String },

    /// No GitHub remote was found on this repository.
    #[error("no GitHub remote found")]
    #[diagnostic(
        code(stakk::remote::no_github),
        help("Make sure this repository has a GitHub remote configured")
    )]
    NoGithubRemote,

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
        help("Pass the bookmark name explicitly: stakk submit <BOOKMARK>")
    )]
    NotInteractive,

    /// User cancelled the interactive prompt.
    #[error("interactive selection cancelled")]
    #[diagnostic(code(stakk::prompt_cancelled))]
    #[expect(
        dead_code,
        reason = "available for callers that want to distinguish cancellation from success"
    )]
    PromptCancelled,

    /// User interrupted with Ctrl-C (exit 130).
    #[error("interrupted")]
    #[diagnostic(code(stakk::interrupted))]
    Interrupted,
}
