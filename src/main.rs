mod cli;
mod error;
mod forge;
mod graph;
mod jj;
mod submit;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::auth::AuthCommands;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Submit(args)) => {
            println!("submit: bookmark '{}' (not yet implemented)", args.bookmark);
        }
        Some(Commands::Auth(args)) => match args.command {
            AuthCommands::Test => {
                println!("auth test: not yet implemented");
            }
            AuthCommands::Setup => {
                println!("auth setup: not yet implemented");
            }
        },
        None => {
            println!("jack — bridge Jujutsu bookmarks to GitHub stacked pull requests.");
            println!("Run `jack --help` for usage information.");
        }
    }

    Ok(())
}
