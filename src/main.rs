mod auth;
mod cli;
mod config;
mod docs;
mod error;
mod forge;
mod graph;
mod jj;
mod markdown;
mod select;
mod show;
mod submit;

use clap::CommandFactory;
use clap::FromArgMatches;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::ShowArgs;
use crate::cli::ShowFormat;
use crate::cli::auth::AuthCommands;
use crate::cli::submit::SubmitArgs;
use crate::error::StakkError::Interrupted;
use crate::error::StakkError::{self};
use crate::forge::Forge;
use crate::forge::comment::StackPlacement;
use crate::jj::Jj;
use crate::jj::remote::parse_github_url;
use crate::jj::remote::parse_remote_url;
use crate::jj::runner::RealJjRunner;
use crate::jj::version::MIN_SUPPORTED_JJ_VERSION;

/// The public GitHub host, always accepted without configuration.
///
/// Lives at the crate root because it is neither a VCS nor a forge concept:
/// `jj::remote` uses it to gate remote hosts, `auth` to pick which token
/// environment variables apply. Either module owning it would make the other
/// depend on it for no reason.
pub const GITHUB_COM: &str = "github.com";

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        if matches!(e, Interrupted) {
            std::process::exit(130);
        }
        eprintln!("{:?}", miette::Report::new(e));
        std::process::exit(1);
    }
}

async fn run() -> Result<(), StakkError> {
    let config_path = config::pre_parse_config_path();
    let config = config::Config::load(config_path)?;
    let cmd = cli::apply_config_defaults(config.clone(), Cli::command());
    let cli = Cli::from_arg_matches(&cmd.get_matches())?;

    // Warn about an outdated jj for commands that shell out to it. Commands that
    // never touch jj (completions, docs, `auth setup`) skip the check.
    let runs_jj = match &cli.command {
        Some(Commands::Completions { .. } | Commands::Docs { .. }) => false,
        Some(Commands::Auth(args)) => matches!(args.command, AuthCommands::Test),
        _ => true, // Submit, Show, and None (= submit) all use jj.
    };
    if runs_jj {
        warn_if_jj_too_old().await;
    }

    // The extra host to treat as GitHub. CLI, STAKK_GITHUB_HOST and the config
    // file are resolved by clap; GH_HOST is the last resort so an existing
    // GitHub CLI setup needs no stakk-specific configuration.
    let github_host = cli
        .github_host
        .clone()
        .or_else(|| std::env::var("GH_HOST").ok().filter(|h| !h.is_empty()));
    let github_host = github_host.as_deref();

    match cli.command {
        Some(Commands::Submit(args)) => {
            submit_bookmark(&args, github_host).await?;
        }
        Some(Commands::Auth(args)) => match args.command {
            AuthCommands::Test => {
                auth_test(github_host).await?;
            }
            AuthCommands::Setup => {
                auth_setup();
            }
        },
        Some(Commands::Show(args)) => {
            show_status(&args, github_host).await?;
        }
        Some(Commands::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "stakk", &mut std::io::stdout());
        }
        Some(Commands::Docs { topic }) => {
            docs::print(topic);
        }
        None => {
            // A bare `stakk` means `stakk submit`. Its arguments come from a
            // clap parse of the synthetic argv `stakk submit` rather than from
            // a hand-built value, so clap defaults, `STAKK_*` environment
            // variables and config-injected defaults all reach it. The global
            // flags are unaffected: `--config` is pre-parsed from the real
            // argv, and `github_host` comes from the real parse above.
            let args = cli::default_submit_args(config).unwrap_or_else(|e| e.exit());
            submit_bookmark(&args, github_host).await?;
        }
    }

    Ok(())
}

/// Warn (to stderr) if the installed jj is older than the minimum supported
/// version.
///
/// Never fails the command: if jj can't be run, or its version output can't be
/// parsed (e.g. an unusual dev build), this stays silent. A genuine jj problem
/// surfaces moments later with a more specific diagnostic.
async fn warn_if_jj_too_old() {
    let jj = Jj::new(RealJjRunner);
    if let Ok(Some(version)) = jj.version().await
        && version < MIN_SUPPORTED_JJ_VERSION
    {
        eprintln!(
            "Warning: jj {version} is older than the minimum supported version \
             ({MIN_SUPPORTED_JJ_VERSION}). stakk may not work correctly — consider upgrading jj."
        );
    }
}

async fn auth_test(github_host: Option<&str>) -> Result<(), StakkError> {
    // The remote comes first: its host decides which token to ask for.
    let (_, github_repo) = resolve_github_remote(None, github_host).await?;
    println!("Host: {}", github_repo.host);

    let auth_token = auth::resolve_token(&github_repo.host).await?;
    println!("Authentication source: {}", auth_token.source);

    let forge = forge::github::GitHubForge::new(
        &auth_token.token,
        github_repo.owner.clone(),
        github_repo.repo.clone(),
        github_repo.api_base_uri().as_deref(),
    )?;

    let username = forge.get_authenticated_user().await?;
    println!("Authenticated as: {username}");

    Ok(())
}

fn auth_setup() {
    println!("stakk resolves a GitHub token for the host your remote points at.\n");
    println!("For github.com, in this order:\n");
    println!("  1. GitHub CLI:    Run `gh auth login` to authenticate.");
    println!("                    This is the recommended method.\n");
    println!("  2. GITHUB_TOKEN:  Set the GITHUB_TOKEN environment variable");
    println!("                    to a personal access token with `repo` scope.\n");
    println!("  3. GH_TOKEN:      Set the GH_TOKEN environment variable");
    println!("                    (same as GITHUB_TOKEN, alternative name).\n");
    println!("For a GitHub Enterprise Server host, name the host first with");
    println!("--github-host, STAKK_GITHUB_HOST, github_host in stakk.toml, or");
    println!("GH_HOST. stakk then resolves the token in this order:\n");
    println!("  1. GitHub CLI:    Run `gh auth login --hostname <host>`.\n");
    println!("  2. GH_ENTERPRISE_TOKEN, then GITHUB_ENTERPRISE_TOKEN.\n");
    println!("To verify: run `stakk auth test`");
}

/// Submits a selection of bookmarks as stacked pull requests using the
/// three-phase pipeline: analyze, plan, execute.
async fn submit_bookmark(args: &SubmitArgs, github_host: Option<&str>) -> Result<(), StakkError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    let jj = Jj::new(RealJjRunner);

    // Resolve the remote before the token: its host decides which token to ask
    // for.
    pb.set_message("Resolving GitHub remote...");
    let (remote_name, github_repo) = resolve_github_remote(Some(&args.remote), github_host).await?;

    pb.set_message("Resolving authentication...");
    let auth_token = auth::resolve_token(&github_repo.host).await?;

    let forge = forge::github::GitHubForge::new(
        &auth_token.token,
        github_repo.owner.clone(),
        github_repo.repo.clone(),
        github_repo.api_base_uri().as_deref(),
    )?;

    // Build the change graph.
    pb.set_message("Building change graph...");
    let change_graph =
        graph::build_change_graph(&jj, &args.graph.bookmarks_revset, &args.graph.heads_revset)
            .await?;

    pb.set_message("Detecting default branch...");
    let default_branch = jj.get_default_branch().await?;

    pb.finish_and_clear();

    // Phase 1: Analyze. Selection always comes from the same place: the
    // interactive TUI when no selection flag is given, the explicit
    // --keep/--new/... marks otherwise. Both yield explicit assignments on a
    // selected path, and the analysis is built directly from those, so new
    // bookmarks need not exist yet — they are created in the execute phase,
    // which keeps --dry-run free of side effects.
    let spec = select::explicit::SelectionSpec::from_args(args)?;
    // Every local bookmark name in the repo, not just the ones on the graph: a
    // new bookmark must not collide with trunk's own bookmark or with anything
    // the bookmarks revset filtered out.
    let reserved_names = jj.get_local_bookmark_names().await?;
    let selection = if spec.is_empty() {
        select::resolve_bookmark_interactively(
            &change_graph,
            args.bookmark_command.as_deref(),
            args.auto_prefix.as_deref(),
            &reserved_names,
        )?
    } else {
        Some(
            select::explicit::resolve_bookmarks_explicitly(
                &change_graph,
                &spec,
                args.auto_prefix.as_deref(),
                args.bookmark_command.as_deref(),
                &reserved_names,
            )
            .await?,
        )
    };
    let (analysis, bookmark_creations) = match selection {
        Some(result) => {
            let analysis = submit::analysis_from_selection(
                &result.path,
                &result.assignments,
                &default_branch,
            )?;
            let creations: Vec<submit::BookmarkCreation> = result
                .assignments
                .iter()
                .filter(|a| a.is_new)
                .map(|a| submit::BookmarkCreation {
                    bookmark_name: a.bookmark_name.clone(),
                    change_id: a.change_id.clone(),
                    short_change_id: a.short_change_id.clone(),
                })
                .collect();
            (analysis, creations)
        }
        None => return Ok(()),
    };

    // Phase 2: Plan.
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("Checking for existing pull requests...");
    let plan = submit::create_submission_plan(
        &analysis,
        bookmark_creations,
        &forge,
        &remote_name,
        args.pr_mode,
        args.sync_pr_content,
        args.trailers,
    )
    .await?;

    pb.finish_and_clear();

    // Print the plan.
    if args.dry_run {
        println!("DRY RUN — no changes will be made.\n");
    }
    println!("{plan}");

    if args.dry_run {
        return Ok(());
    }

    // Load template. In `none`/`ignore` placement no stack content is ever
    // rendered, so a custom template is neither read nor compiled — a broken
    // or missing one must not fail a submission that will not use it.
    let template_source = match (&args.template_path, args.stack_placement) {
        (Some(path), StackPlacement::Comment | StackPlacement::Body) => Some(
            std::fs::read_to_string(path).map_err(|e| StakkError::TemplateLoadFailed {
                path: path.clone(),
                reason: e.to_string(),
            })?,
        ),
        _ => None,
    };
    let comment_env = forge::comment::build_comment_env(template_source.as_deref())?;

    // Phase 3: Execute. The header separates the plan from the result lines
    // printed during execution.
    println!("\nExecuting:");
    let result =
        submit::execute_submission_plan(&plan, &jj, &forge, &comment_env, args.stack_placement)
            .await?;

    println!("\nSubmitted {} bookmark(s).", result.stack_entries.len());

    Ok(())
}

/// Resolve the GitHub remote from jj's remote list.
///
/// If `preferred` is given, looks for that specific remote name. Otherwise,
/// falls back to the first remote with a GitHub URL.
///
/// `github_host` is an extra host to accept besides github.com.
///
/// Returns the remote name and parsed `GitHubRepo`.
async fn resolve_github_remote(
    preferred: Option<&str>,
    github_host: Option<&str>,
) -> Result<(String, jj::remote::GitHubRepo), StakkError> {
    let jj = Jj::new(RealJjRunner);
    let remotes = jj.get_git_remote_list().await?;

    if let Some(name) = preferred {
        if let Some(remote) = remotes.iter().find(|r| r.name == name) {
            if let Some(repo) = parse_github_url(&remote.url, github_host) {
                return Ok((remote.name.clone(), repo));
            }
            // An owner/repo URL on an unconfigured host gets a diagnostic that
            // names the host, rather than the generic "not a GitHub URL".
            if let Some(parsed) = parse_remote_url(&remote.url) {
                return Err(StakkError::RemoteHostNotConfigured {
                    name: name.to_string(),
                    url: remote.url.clone(),
                    host: parsed.host,
                });
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
        if let Some(repo) = parse_github_url(&remote.url, github_host) {
            return Ok((remote.name.clone(), repo));
        }
    }

    Err(StakkError::NoGithubRemote)
}

async fn show_status(args: &ShowArgs, github_host: Option<&str>) -> Result<(), StakkError> {
    // No spinner in json mode: machine-readable output stays quiet.
    let spinner = matches!(args.format, ShowFormat::Pretty).then(|| {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb.set_message("Loading repository status...");
        pb
    });

    let jj = Jj::new(RealJjRunner);

    let default_branch = jj.get_default_branch().await?;

    let remotes = jj.get_git_remote_list().await?;

    let change_graph =
        graph::build_change_graph(&jj, &args.graph.bookmarks_revset, &args.graph.heads_revset)
            .await?;

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    let data = show::ShowData {
        default_branch: &default_branch,
        remotes: &remotes,
        graph: &change_graph,
        github_host,
    };
    match args.format {
        ShowFormat::Pretty => print!("{}", show::render_pretty(&data, console::colors_enabled())),
        ShowFormat::Json => print!("{}", show::render_json(&data)),
    }

    Ok(())
}
