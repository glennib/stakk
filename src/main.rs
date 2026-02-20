mod auth;
mod cli;
mod error;
mod forge;
mod graph;
mod jj;
mod select;
mod submit;

use clap::Parser;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::auth::AuthCommands;
use crate::cli::submit::SubmitArgs;
use crate::error::StakkError;
use crate::forge::Forge;
use crate::jj::Jj;
use crate::jj::remote::parse_github_url;
use crate::jj::runner::RealJjRunner;
use crate::select::collect_stack_choices;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{:?}", miette::Report::new(e));
        std::process::exit(1);
    }
}

async fn run() -> Result<(), StakkError> {
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
        Some(Commands::Show) | None => {
            show_status().await?;
        }
    }

    Ok(())
}

async fn auth_test() -> Result<(), StakkError> {
    let auth_token = auth::resolve_token().await?;
    println!("Authentication source: {}", auth_token.source);

    let (_, github_repo) = resolve_github_remote(None).await?;

    let forge =
        forge::github::GitHubForge::new(&auth_token.token, github_repo.owner, github_repo.repo)?;

    let username = forge.get_authenticated_user().await?;
    println!("Authenticated as: {username}");

    Ok(())
}

fn auth_setup() {
    println!("stakk resolves GitHub authentication in this order:\n");
    println!("  1. GitHub CLI:    Run `gh auth login` to authenticate.");
    println!("                    This is the recommended method.\n");
    println!("  2. GITHUB_TOKEN:  Set the GITHUB_TOKEN environment variable");
    println!("                    to a personal access token with `repo` scope.\n");
    println!("  3. GH_TOKEN:      Set the GH_TOKEN environment variable");
    println!("                    (same as GITHUB_TOKEN, alternative name).\n");
    println!("To verify: run `stakk auth test`");
}

/// Submit a bookmark as a stacked pull request using the three-phase pipeline:
/// analyze, plan, execute.
async fn submit_bookmark(args: &SubmitArgs) -> Result<(), StakkError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    pb.set_message("Resolving authentication...");
    let jj = Jj::new(RealJjRunner);

    // Resolve auth and remote.
    let auth_token = auth::resolve_token().await?;

    pb.set_message("Resolving GitHub remote...");
    let (remote_name, github_repo) = resolve_github_remote(Some(&args.remote)).await?;

    let forge = forge::github::GitHubForge::new(
        &auth_token.token,
        github_repo.owner.clone(),
        github_repo.repo.clone(),
    )?;

    // Build the change graph.
    pb.set_message("Building change graph...");
    let change_graph = graph::build_change_graph(&jj).await?;

    pb.set_message("Detecting default branch...");
    let default_branch = jj.get_default_branch().await?;

    // Resolve bookmark: explicit argument or interactive selection.
    pb.finish_and_clear();

    let bookmark = match &args.bookmark {
        Some(name) => name.clone(),
        None => match select::resolve_bookmark_interactively(&change_graph)? {
            Some(name) => name,
            None => return Ok(()),
        },
    };

    // Phase 1: Analyze.
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("Analyzing submission...");
    let analysis = submit::analyze_submission(&bookmark, &change_graph, &default_branch)?;

    // Phase 2: Plan.
    pb.set_message("Checking for existing pull requests...");
    let plan = submit::create_submission_plan(&analysis, &forge, &remote_name, args.draft).await?;

    pb.finish_and_clear();

    // Print the plan.
    if args.dry_run {
        println!("DRY RUN — no changes will be made.\n");
    }
    println!("{plan}");

    if args.dry_run {
        return Ok(());
    }

    // Phase 3: Execute.
    let result = submit::execute_submission_plan(&plan, &jj, &forge).await?;

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
) -> Result<(String, jj::remote::GitHubRepo), StakkError> {
    let jj = Jj::new(RealJjRunner);
    let remotes = jj.get_git_remote_list().await?;

    if let Some(name) = preferred {
        if let Some(remote) = remotes.iter().find(|r| r.name == name) {
            if let Some(repo) = parse_github_url(&remote.url) {
                return Ok((remote.name.clone(), repo));
            }
            return Err(StakkError::RemoteNotGithub {
                name: name.to_string(),
                url: remote.url.clone(),
            });
        }
        return Err(StakkError::RemoteNotFound {
            name: name.to_string(),
        });
    }

    for remote in &remotes {
        if let Some(repo) = parse_github_url(&remote.url) {
            return Ok((remote.name.clone(), repo));
        }
    }

    Err(StakkError::NoGithubRemote)
}

async fn show_status() -> Result<(), StakkError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("Loading repository status...");

    let jj = Jj::new(RealJjRunner);

    let default_branch = jj.get_default_branch().await?;

    let remotes = jj.get_git_remote_list().await?;

    let change_graph = graph::build_change_graph(&jj).await?;

    pb.finish_and_clear();

    println!("Default branch: {default_branch}");
    for remote in &remotes {
        let github = parse_github_url(&remote.url)
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        println!("Remote: {} {}{}", remote.name, remote.url, github);
    }

    if change_graph.stacks.is_empty() {
        println!("\nNo bookmark stacks found.");
    } else {
        let choices = collect_stack_choices(&change_graph);
        println!("\nStacks ({} found):", choices.len());
        for choice in &choices {
            println!("  {choice}");
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
