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
    #[arg(long, env = "STAKK_DRAFT")]
    pub draft: bool,

    /// Git remote to push to.
    #[arg(long, default_value = "origin", env = "STAKK_REMOTE")]
    pub remote: String,

    /// Path to a custom minijinja template for stack comments.
    #[arg(long, env = "STAKK_TEMPLATE")]
    pub template: Option<String>,
}

impl Default for SubmitArgs {
    fn default() -> Self {
        Self {
            bookmark: None,
            dry_run: false,
            draft: false,
            remote: "origin".to_string(),
            template: None,
        }
    }
}
