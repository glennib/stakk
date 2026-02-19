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
use crate::forge::Forge;
use crate::forge::comment::StackCommentData;
use crate::forge::comment::StackEntry;
use crate::forge::comment::find_stack_comment;
use crate::forge::comment::format_stack_comment;
use crate::jj::Jj;
use crate::jj::remote::parse_github_url;
use crate::jj::runner::RealJjRunner;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Submit(args)) => {
            submit_bookmark(&args.bookmark).await?;
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

    let (_, github_repo) = resolve_github_remote().await?;

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

/// Minimal submit for a single bookmark. Pushes the bookmark, creates or
/// finds the PR, and adds/updates a stack comment.
///
/// This is temporary scaffolding — M5 replaces it with the full three-phase
/// submission workflow.
async fn submit_bookmark(bookmark: &str) -> Result<()> {
    let jj = Jj::new(RealJjRunner);

    // Resolve auth and remote.
    let auth_token = auth::resolve_token()
        .await
        .context("failed to resolve GitHub authentication")?;
    let (remote_name, github_repo) = resolve_github_remote().await?;

    let forge = forge::github::GitHubForge::new(
        &auth_token.token,
        github_repo.owner.clone(),
        github_repo.repo.clone(),
    )
    .context("failed to create GitHub client")?;

    // Build the change graph and find the stack containing this bookmark.
    let change_graph = graph::build_change_graph(&jj)
        .await
        .context("failed to build change graph")?;

    let stack = change_graph
        .stacks
        .iter()
        .find(|s| {
            s.segments
                .iter()
                .any(|seg| seg.bookmark_names.contains(&bookmark.to_string()))
        })
        .with_context(|| format!("bookmark '{bookmark}' not found in any stack"))?;

    // Find the target bookmark's index in the stack.
    let target_index = stack
        .segments
        .iter()
        .position(|seg| seg.bookmark_names.contains(&bookmark.to_string()))
        .expect("bookmark was found in stack above");

    // Submit all segments from trunk up to and including the target.
    let relevant_segments = &stack.segments[..=target_index];

    let default_branch = jj
        .get_default_branch()
        .await
        .context("failed to detect default branch")?;

    // Push all relevant bookmarks and collect PR info.
    let mut stack_entries: Vec<StackEntry> = Vec::new();

    for (i, segment) in relevant_segments.iter().enumerate() {
        let bm = segment
            .bookmark_names
            .first()
            .context("segment has no bookmark name")?;

        // Push the bookmark.
        println!("Pushing bookmark: {bm}");
        jj.push_bookmark(bm, &remote_name)
            .await
            .with_context(|| format!("failed to push bookmark '{bm}'"))?;

        // Determine the base branch.
        let base = if i == 0 {
            default_branch.clone()
        } else {
            relevant_segments[i - 1]
                .bookmark_names
                .first()
                .context("parent segment has no bookmark name")?
                .clone()
        };

        // Find or create the PR.
        let pr = match forge
            .find_pr_for_branch(bm)
            .await
            .with_context(|| format!("failed to check for existing PR for '{bm}'"))?
        {
            Some(existing) => {
                println!(
                    "  Found existing PR #{}: {}",
                    existing.number, existing.html_url
                );
                // Update base if it changed.
                if existing.base_ref != base {
                    println!("  Updating base: {} -> {base}", existing.base_ref);
                    forge
                        .update_pr_base(existing.number, &base)
                        .await
                        .context("failed to update PR base")?;
                }
                existing
            }
            None => {
                let title = segment
                    .commits
                    .first()
                    .map(|c| {
                        c.description
                            .lines()
                            .next()
                            .unwrap_or(&c.description)
                            .to_string()
                    })
                    .unwrap_or_else(|| bm.to_string());

                println!("  Creating PR: {title}");
                let pr = forge
                    .create_pr(forge::CreatePrParams {
                        title,
                        head: bm.to_string(),
                        base,
                        body: None,
                        draft: false,
                    })
                    .await
                    .with_context(|| format!("failed to create PR for '{bm}'"))?;
                println!("  Created PR #{}: {}", pr.number, pr.html_url);
                pr
            }
        };

        stack_entries.push(StackEntry {
            bookmark_name: bm.to_string(),
            pr_url: pr.html_url.clone(),
            pr_number: pr.number,
        });
    }

    // Add/update stack comments on all PRs.
    let comment_data = StackCommentData {
        version: 0,
        stack: stack_entries.clone(),
    };

    for (i, entry) in stack_entries.iter().enumerate() {
        let body = format_stack_comment(&comment_data, i);
        let existing_comments = forge
            .list_comments(entry.pr_number)
            .await
            .with_context(|| format!("failed to list comments on PR #{}", entry.pr_number))?;

        if let Some(existing) = find_stack_comment(&existing_comments) {
            forge
                .update_comment(existing.id, &body)
                .await
                .with_context(|| {
                    format!("failed to update stack comment on PR #{}", entry.pr_number)
                })?;
        } else {
            forge
                .create_comment(entry.pr_number, &body)
                .await
                .with_context(|| {
                    format!("failed to create stack comment on PR #{}", entry.pr_number)
                })?;
        }
    }

    println!("\nSubmitted {} bookmark(s).", relevant_segments.len());

    Ok(())
}

/// Resolve the GitHub remote from jj's remote list.
///
/// Returns the remote name and parsed `GitHubRepo`.
async fn resolve_github_remote() -> Result<(String, jj::remote::GitHubRepo)> {
    let jj = Jj::new(RealJjRunner);
    let remotes = jj
        .get_git_remote_list()
        .await
        .context("failed to list git remotes")?;

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
