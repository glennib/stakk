mod cli;
mod error;
mod forge;
mod graph;
mod jj;
mod submit;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::auth::AuthCommands;
use crate::jj::Jj;
use crate::jj::remote::parse_github_url;
use crate::jj::runner::RealJjRunner;

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
            show_status().await?;
        }
    }

    Ok(())
}

async fn show_status() -> Result<()> {
    let jj = Jj::new(RealJjRunner);

    let default_branch = jj
        .get_default_branch()
        .await
        .context("failed to detect default branch")?;
    println!("Default branch: {default_branch}");

    let remotes = jj
        .get_git_remote_list()
        .await
        .context("failed to list git remotes")?;
    for remote in &remotes {
        let github = parse_github_url(&remote.url)
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        println!("Remote: {} {}{}", remote.name, remote.url, github);
    }

    let bookmarks = jj
        .get_my_bookmarks()
        .await
        .context("failed to list bookmarks")?;
    if bookmarks.is_empty() {
        println!("No bookmarks found (matching mine() ~ trunk()).");
    } else {
        println!("\nBookmarks:");
        for b in &bookmarks {
            let synced = if b.synced { " [synced]" } else { "" };
            println!(
                "  {} (commit {}, change {}){synced}",
                b.name,
                &b.commit_id[..12.min(b.commit_id.len())],
                &b.change_id[..12.min(b.change_id.len())],
            );
        }
    }

    Ok(())
}
