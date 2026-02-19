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

    let change_graph = graph::build_change_graph(&jj)
        .await
        .context("failed to build change graph")?;

    if change_graph.stacks.is_empty() {
        println!("\nNo bookmark stacks found.");
    } else {
        println!("\nStacks ({} found):", change_graph.stacks.len());
        for (i, stack) in change_graph.stacks.iter().enumerate() {
            println!("  Stack {}:", i + 1);
            for segment in &stack.segments {
                let names = segment.bookmark_names.join(", ");
                let commit_count = segment.commits.len();
                let desc = segment
                    .commits
                    .first()
                    .map(|c| c.description.trim())
                    .unwrap_or("(no description)");
                println!("    {names} ({commit_count} commit(s)): {desc}");
            }
        }

        if change_graph.excluded_bookmark_count > 0 {
            println!(
                "\n  ({} bookmark(s) excluded due to merge commits)",
                change_graph.excluded_bookmark_count,
            );
        }
    }

    Ok(())
}
