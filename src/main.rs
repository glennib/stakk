mod auth;
mod cli;
mod error;
mod forge;
mod graph;
mod jj;
mod submit;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::auth::AuthCommands;
use crate::cli::submit::SubmitArgs;
use crate::forge::Forge;
use crate::jj::Jj;
use crate::jj::remote::parse_github_url;
use crate::jj::runner::RealJjRunner;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Submit(args)) => {
            submit_bookmark(&args).await?;
        }
        Some(Commands::Auth(args)) => match args.command {
            AuthCommands::Test => {
                auth_test().await?;
            }
            AuthCommands::Setup => {
                auth_setup();
            }
        },
        None => {
            show_status().await?;
        }
    }

    Ok(())
}

async fn auth_test() -> Result<()> {
    let auth_token = auth::resolve_token()
        .await
        .context("failed to resolve GitHub authentication")?;
    println!("Authentication source: {}", auth_token.source);

    let (_, github_repo) = resolve_github_remote(None).await?;

    let forge =
        forge::github::GitHubForge::new(&auth_token.token, github_repo.owner, github_repo.repo)
            .context("failed to create GitHub client")?;

    let username = forge
        .get_authenticated_user()
        .await
        .context("failed to validate GitHub token")?;
    println!("Authenticated as: {username}");

    Ok(())
}

fn auth_setup() {
    println!("jack resolves GitHub authentication in this order:\n");
    println!("  1. GitHub CLI:    Run `gh auth login` to authenticate.");
    println!("                    This is the recommended method.\n");
    println!("  2. GITHUB_TOKEN:  Set the GITHUB_TOKEN environment variable");
    println!("                    to a personal access token with `repo` scope.\n");
    println!("  3. GH_TOKEN:      Set the GH_TOKEN environment variable");
    println!("                    (same as GITHUB_TOKEN, alternative name).\n");
    println!("To verify: run `jack auth test`");
}

/// Submit a bookmark as a stacked pull request using the three-phase pipeline:
/// analyze, plan, execute.
async fn submit_bookmark(args: &SubmitArgs) -> Result<()> {
    let jj = Jj::new(RealJjRunner);

    // Resolve auth and remote.
    let auth_token = auth::resolve_token()
        .await
        .context("failed to resolve GitHub authentication")?;
    let (remote_name, github_repo) = resolve_github_remote(Some(&args.remote)).await?;

    let forge = forge::github::GitHubForge::new(
        &auth_token.token,
        github_repo.owner.clone(),
        github_repo.repo.clone(),
    )
    .context("failed to create GitHub client")?;

    // Build the change graph.
    let change_graph = graph::build_change_graph(&jj)
        .await
        .context("failed to build change graph")?;

    let default_branch = jj
        .get_default_branch()
        .await
        .context("failed to detect default branch")?;

    // Phase 1: Analyze.
    let analysis = submit::analyze_submission(&args.bookmark, &change_graph, &default_branch)
        .context("failed to analyze submission")?;

    // Phase 2: Plan.
    let plan = submit::create_submission_plan(&analysis, &forge, &remote_name)
        .await
        .context("failed to create submission plan")?;

    // Print the plan.
    println!("{plan}");

    if args.dry_run {
        return Ok(());
    }

    // Phase 3: Execute.
    let result = submit::execute_submission_plan(&plan, &jj, &forge)
        .await
        .context("failed to execute submission plan")?;

    println!("\nSubmitted {} bookmark(s).", result.stack_entries.len());

    Ok(())
}

/// Resolve the GitHub remote from jj's remote list.
///
/// If `preferred` is given, looks for that specific remote name. Otherwise,
/// falls back to the first remote with a GitHub URL.
///
/// Returns the remote name and parsed `GitHubRepo`.
async fn resolve_github_remote(
    preferred: Option<&str>,
) -> Result<(String, jj::remote::GitHubRepo)> {
    let jj = Jj::new(RealJjRunner);
    let remotes = jj
        .get_git_remote_list()
        .await
        .context("failed to list git remotes")?;

    if let Some(name) = preferred {
        if let Some(remote) = remotes.iter().find(|r| r.name == name) {
            if let Some(repo) = parse_github_url(&remote.url) {
                return Ok((remote.name.clone(), repo));
            }
            bail!("remote '{name}' is not a GitHub URL: {}", remote.url);
        }
        bail!("remote '{name}' not found");
    }

    for remote in &remotes {
        if let Some(repo) = parse_github_url(&remote.url) {
            return Ok((remote.name.clone(), repo));
        }
    }

    bail!("no GitHub remote found; is this a GitHub repository?")
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
