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

    /// An error from the non-interactive selection flags.
    #[error(transparent)]
    #[diagnostic(transparent)]
    ExplicitSelection(#[from] crate::select::explicit::ExplicitSelectionError),

    /// A configuration error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] ConfigError),

    /// The specified remote is not a GitHub URL.
    #[error("remote '{name}' is not a GitHub URL: {url}")]
    #[diagnostic(
        code(stakk::remote::not_github),
        help(
            "stakk expects an owner/repo remote URL, e.g. git@github.com:owner/repo.git; for a \
             GitHub Enterprise Server host, also set --github-host"
        )
    )]
    RemoteNotGithub { name: String, url: String },

    /// The remote is an owner/repo URL, but on a host stakk has not been told
    /// to treat as GitHub.
    #[error("remote '{name}' is on host '{host}', which is not a configured GitHub host: {url}")]
    #[diagnostic(
        code(stakk::remote::host_not_configured),
        help(
            "if {host} is a GitHub Enterprise Server host, name it with --github-host {host}, \
             STAKK_GITHUB_HOST, github_host in stakk.toml, or GH_HOST"
        )
    )]
    RemoteHostNotConfigured {
        name: String,
        url: String,
        host: String,
    },

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
        help(
            "make sure this repository has a GitHub remote configured; for a GitHub Enterprise \
             Server host, name it with --github-host, STAKK_GITHUB_HOST, github_host in \
             stakk.toml, or GH_HOST"
        )
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
        help(
            "select explicitly instead: stakk submit --keep <BOOKMARK> / --new <REV>[=<NAME>] — \
             run `stakk docs scripting` for the full non-interactive workflow"
        )
    )]
    NotInteractive,

    /// User interrupted with Ctrl-C (exit 130).
    #[error("interrupted")]
    #[diagnostic(code(stakk::interrupted))]
    Interrupted,
}
