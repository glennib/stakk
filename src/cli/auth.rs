use clap::Args;
use clap::Subcommand;

/// Arguments for the `auth` subcommand.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommands,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommands {
    /// Test that authentication is working.
    Test,
    /// Print instructions for setting up authentication.
    Setup,
}
