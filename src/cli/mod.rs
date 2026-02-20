pub mod auth;
pub mod submit;

use clap::Parser;
use clap::Subcommand;
use clap_complete::Shell;

use crate::cli::auth::AuthArgs;
use crate::cli::submit::SubmitArgs;

/// stakk — bridge Jujutsu bookmarks to GitHub stacked pull requests.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Submit bookmarks as GitHub pull requests (default when no command
    /// given).
    Submit(SubmitArgs),
    /// Manage authentication.
    Auth(AuthArgs),
    /// Show repository status and bookmark stacks.
    Show,
    /// Generate shell completions for the given shell.
    Completions {
        /// The shell to generate completions for.
        shell: Shell,
    },
}
