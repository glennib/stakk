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
mod submit;

use clap::CommandFactory;
use clap::FromArgMatches;

use crate::cli::Cli;
use crate::cli::Commands;
use crate::cli::GraphArgs;
use crate::cli::GraphFormat;
use crate::cli::submit::SubmitArgs;
use crate::error::StakkError::Interrupted;
use crate::error::StakkError::{self};
use crate::forge::Forge;
use crate::forge::ForgeKind;
use crate::forge::comment::StackPlacement;
use crate::forge::detect;
use crate::forge::detect::Classification;
use crate::forge::detect::DetectError;
use crate::forge::detect::HostRules;
use crate::forge::detect::ProbeOutcome;
use crate::forge::forgejo::transport::ReqwestTransport;
use crate::jj::Jj;
use crate::jj::remote::RemoteRepo;
use crate::jj::remote::parse_remote_url;
use crate::jj::runner::RealJjRunner;
use crate::jj::version::MIN_SUPPORTED_JJ_VERSION;

/// The public GitHub host, GitHub without any configuration.
///
/// Lives at the crate root because it is neither a VCS nor a forge concept:
/// `forge::detect` has it built in, `auth` uses it to pick which token
/// environment variables apply, and `jj::remote` to leave octocrab its default
/// API base. Any one of those owning it would make the others depend on it
/// for no reason.
pub const GITHUB_COM: &str = "github.com";

/// The public Forgejo host, Forgejo without any configuration.
///
/// Lives next to [`GITHUB_COM`] for the same reason it does: the public hosts
/// are a crate-level fact that `forge::detect` has built in, not a property of
/// the forge client, and keeping the two side by side is what shows they play
/// the same role for their forge.
pub const CODEBERG_ORG: &str = "codeberg.org";

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

    // Warn about environment variables stakk stopped reading, for the two paths
    // that consume submit args. `graph`, `docs` and `completions` never read
    // them and never did, so they pay nothing — not even a stray stderr line in
    // a shell that evaluates `stakk completions zsh`.
    if matches!(&cli.command, Some(Commands::Submit(_)) | None) {
        config::warn_removed_env_vars();
    }

    // Warn about an outdated jj for commands that shell out to it. Commands
    // that never touch jj (completions, docs) skip the check.
    let runs_jj = match &cli.command {
        Some(Commands::Completions { .. } | Commands::Docs { .. }) => false,
        _ => true, // Submit, Graph, and None (= submit) all use jj.
    };
    if runs_jj {
        warn_if_jj_too_old().await;
    }

    // The host table, layered lowest first: GH_HOST (the GitHub CLI's own
    // setting, so an existing gh setup needs nothing further), `hosts` from
    // the config file, then `--host`/STAKK_HOSTS. The config layer is merged
    // here rather than injected as a clap default: a default is replaced
    // wholesale by the first typed `--host`, while the table merges per host.
    let gh_host = std::env::var("GH_HOST").ok();
    let rules = HostRules::layered(gh_host.as_deref(), config.hosts.as_ref(), &cli.hosts);

    match cli.command {
        Some(Commands::Submit(args)) => {
            submit_bookmark(&args, cli.forge, &rules).await?;
        }
        Some(Commands::Graph(args)) => {
            print_graph(&args).await?;
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
            // argv, and `--forge` and `--host` come from the real parse above.
            let args = cli::default_submit_args(config).unwrap_or_else(|e| e.exit());
            submit_bookmark(&args, cli.forge, &rules).await?;
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

/// Submits a selection of bookmarks as stacked pull requests using the
/// three-phase pipeline: analyze, plan, execute.
///
/// Resolves the remote, then the forge for its host, then the token, and
/// builds the concrete forge client for that kind to hand to
/// [`run_submission`]. `Forge` uses return-position `impl Future`, so it is
/// not dyn-compatible: the two forges are two match arms constructing two
/// types, and the pipeline behind them is generic rather than an enum
/// forwarding every method.
///
/// `explicit` is `--forge`, which skips the host lookup; `rules` is the host
/// table it falls back to.
async fn submit_bookmark(
    args: &SubmitArgs,
    explicit: Option<ForgeKind>,
    rules: &HostRules,
) -> Result<(), StakkError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    // Resolve the remote before the forge and the token: its host decides
    // both.
    pb.set_message("Resolving remote...");
    let (remote_name, repo) = resolve_remote(&args.remote).await?;

    let kind = match detect::classify(&repo.host, explicit, rules) {
        Classification::Forge(kind) => kind,
        Classification::Unsupported(forge) => {
            return Err(DetectError::Unsupported {
                host: repo.host,
                forge,
            }
            .into());
        }
        Classification::Unknown => {
            pb.set_message(format!("Detecting the forge at {}...", repo.host));
            let transport = ReqwestTransport::probing().map_err(DetectError::Client)?;
            match detect::probe(&transport, &repo).await {
                ProbeOutcome::Detected(kind) => {
                    // `suspend`, not `println`: indicatif drops `println`
                    // output while the draw target is hidden (stderr not a
                    // terminal), and the scripted runs that would lose it are
                    // the ones the hint is for.
                    pb.suspend(|| {
                        eprintln!(
                            "  Detected {} at {host}; skip this probe next time with `hosts = {{ \
                             \"{host}\" = \"{kind}\" }}` in stakk.toml or `--host {host}={kind}`",
                            kind.label(),
                            host = repo.host,
                        );
                    });
                    kind
                }
                ProbeOutcome::Unsupported(forge) => {
                    return Err(DetectError::Unsupported {
                        host: repo.host,
                        forge,
                    }
                    .into());
                }
                ProbeOutcome::Ambiguous(report) => {
                    return Err(DetectError::Ambiguous {
                        host: repo.host,
                        report,
                    }
                    .into());
                }
                ProbeOutcome::Unknown(report) => {
                    return Err(DetectError::Unknown {
                        host: repo.host,
                        report,
                    }
                    .into());
                }
            }
        }
    };

    pb.set_message("Resolving authentication...");
    match kind {
        ForgeKind::Github => {
            let auth_token = auth::resolve_token(&repo.host).await?;
            let forge = forge::github::GitHubForge::new(
                &auth_token.token,
                repo.owner.clone(),
                repo.repo.clone(),
                repo.github_api_base_uri().as_deref(),
            )?;
            run_submission(forge, args, &remote_name, pb).await
        }
        ForgeKind::Forgejo => {
            let auth_token = auth::resolve_forgejo_token(&repo.host)?;
            let forge = forge::forgejo::ForgejoForge::new(
                &auth_token.token,
                repo.owner.clone(),
                repo.repo.clone(),
                &repo.forgejo_api_base_uri(),
            )?;
            run_submission(forge, args, &remote_name, pb).await
        }
    }
}

/// Everything from the change graph onward, generic over the forge.
///
/// `pb` is the spinner `submit_bookmark` started for the remote and token
/// steps; it is finished here, once the graph is built.
async fn run_submission<F: Forge>(
    forge: F,
    args: &SubmitArgs,
    remote_name: &str,
    pb: indicatif::ProgressBar,
) -> Result<(), StakkError> {
    let jj = Jj::new(RealJjRunner);

    // Build the change graph.
    pb.set_message("Building change graph...");
    let change_graph = graph::build_change_graph(
        &jj,
        &args.revset.bookmarks_revset,
        &args.revset.heads_revset,
    )
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
                &jj,
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
        remote_name,
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
    // or missing one must not fail a submission that will not use it. Every
    // other placement may render (the autos resolve only at execute time),
    // so the non-rendering placements are the ones enumerated: a placement
    // added later loads the template by default instead of silently
    // ignoring --template-path.
    let template_source = match args.stack_placement {
        StackPlacement::None | StackPlacement::Ignore => None,
        _ => args
            .template_path
            .as_ref()
            .map(|path| {
                std::fs::read_to_string(path).map_err(|e| StakkError::TemplateLoadFailed {
                    path: path.clone(),
                    reason: e.to_string(),
                })
            })
            .transpose()?,
    };
    let comment_env = forge::comment::build_comment_env(template_source.as_deref())?;

    // Phase 3: Execute. The header separates the plan from the result lines
    // printed during execution.
    println!("\nExecuting:");
    let result = submit::execute_submission_plan(
        &plan,
        &jj,
        &forge,
        &comment_env,
        args.stack_placement,
        args.native_stacks,
    )
    .await?;

    println!("\nSubmitted {} bookmark(s).", result.stack_entries.len());

    Ok(())
}

/// Find the remote called `name` in jj's remote list and parse its URL.
///
/// Returns the remote name and the parsed `RemoteRepo`. Which forge the host
/// runs is not decided here — that is `forge::detect`'s job, once the host is
/// known.
async fn resolve_remote(name: &str) -> Result<(String, RemoteRepo), StakkError> {
    let jj = Jj::new(RealJjRunner);
    let remotes = jj.get_git_remote_list().await?;

    let remote =
        remotes
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| StakkError::RemoteNotFound {
                name: name.to_string(),
            })?;
    let repo = parse_remote_url(&remote.url).ok_or_else(|| StakkError::RemoteNotRepoUrl {
        name: name.to_string(),
        url: remote.url.clone(),
    })?;
    Ok((remote.name.clone(), repo))
}

async fn print_graph(args: &GraphArgs) -> Result<(), StakkError> {
    // No spinner in json mode: machine-readable output stays quiet.
    let spinner = matches!(args.format, GraphFormat::Pretty).then(|| {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb.set_message("Loading repository status...");
        pb
    });

    let jj = Jj::new(RealJjRunner);

    let default_branch = jj.get_default_branch().await?;

    let remotes = jj.get_git_remote_list().await?;

    let change_graph = graph::build_change_graph(
        &jj,
        &args.revset.bookmarks_revset,
        &args.revset.heads_revset,
    )
    .await?;

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    let data = graph::output::GraphData {
        default_branch: &default_branch,
        remotes: &remotes,
        graph: &change_graph,
    };
    print!(
        "{}",
        graph::output::render(&data, args.format, console::colors_enabled())
    );

    Ok(())
}
