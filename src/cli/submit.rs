use clap::Args;

/// Arguments for the `submit` subcommand.
#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The bookmark to submit as a pull request. If omitted, shows an
    /// interactive selection.
    pub bookmark: Option<String>,

    /// Show what would be done without actually doing it.
    #[arg(long)]
    pub dry_run: bool,

    /// Create pull requests as drafts.
    #[arg(long)]
    pub draft: bool,

    /// Git remote to push to.
    #[arg(long, default_value = "origin")]
    pub remote: String,
}
