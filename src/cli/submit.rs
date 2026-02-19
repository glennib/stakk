use clap::Args;

/// Arguments for the `submit` subcommand.
#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The bookmark to submit as a pull request.
    pub bookmark: String,
}
