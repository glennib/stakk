pub mod auth;
pub mod graph;
pub mod submit;

use std::path::PathBuf;

use clap::Args;
use clap::Command;
use clap::CommandFactory;
use clap::FromArgMatches;
use clap::Parser;
use clap::Subcommand;
use clap_complete::Shell;

use crate::cli::auth::AuthArgs;
use crate::cli::graph::GraphArgs;
use crate::cli::submit::SubmitArgs;
use crate::config::Config;

/// stakk — bridge Jujutsu bookmarks to GitHub stacked pull requests.
#[derive(Debug, Parser)]
#[command(version, about, after_long_help = env!("CARGO_PKG_REPOSITORY"))]
pub struct Cli {
    /// Path to a config file (overrides automatic discovery).
    ///
    /// Loaded in place of the repo-level stakk.toml; user-level config is
    /// still merged unless inherit = false.
    // Implementation note: this arg exists for --help discoverability only.
    // Config is loaded *before* clap parsing (so config values can be injected
    // as clap defaults), which means clap's parsed value arrives too late.
    // The actual path is resolved by `config::pre_parse_config_path()` from
    // raw `std::env::args()` / `STAKK_CONFIG`.
    #[arg(long, global = true, env = "STAKK_CONFIG", verbatim_doc_comment)]
    pub config: Option<PathBuf>,

    /// Extra host to treat as GitHub, for GitHub Enterprise Server.
    ///
    /// github.com is always accepted. Naming a host here additionally accepts
    /// remotes on that host and talks to its API at https://<host>/api/v3.
    /// Falls back to GH_HOST when unset, so an existing GitHub CLI setup works
    /// without further configuration.
    #[arg(long, global = true, env = "STAKK_GITHUB_HOST", verbatim_doc_comment)]
    pub github_host: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Submit bookmarks as GitHub pull requests (default when no command
    /// given).
    // Boxed: SubmitArgs is by far the largest payload (clippy
    // large_enum_variant).
    Submit(Box<SubmitArgs>),
    /// Manage authentication.
    Auth(AuthArgs),
    /// Show repository status and bookmark stacks.
    Show(ShowArgs),
    /// Generate shell completions for the given shell.
    Completions {
        /// The shell to generate completions for.
        shell: Shell,
    },
    /// Print stakk's bundled documentation, or list the available topics.
    Docs {
        /// Topic to print. Omit to list the available topics.
        #[arg(value_enum)]
        topic: Option<DocTopic>,
    },
}

/// A topic of the documentation bundled into the binary.
///
/// The variant docs below are load-bearing: clap surfaces them as
/// possible-value help, and `docs::index` reads them back to build the topic
/// list, so they cannot drift from each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DocTopic {
    /// Non-interactive submission, for scripts and coding agents.
    Scripting,
    /// Config files, precedence, and environment variables.
    Config,
    /// Stack info placement and stack comment templates.
    Template,
}

/// Output format for the show subcommand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ShowFormat {
    /// Human-readable jj-log-style graph of all bookmark stacks.
    #[default]
    Pretty,
    /// Machine-readable, schema-versioned JSON document.
    Json,
}

/// Arguments for the show subcommand.
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Output format.
    ///
    /// pretty renders a fully expanded jj-log-style commit graph: short
    /// change id, bookmarks and description summary per commit.
    ///
    /// json describes every stack, segment and commit for scripts and
    /// agents. Its identifiers (short_change_id, bookmark names) can be
    /// passed directly to `stakk submit`.
    #[arg(long, default_value = "pretty", value_enum, verbatim_doc_comment)]
    pub format: ShowFormat,

    #[command(flatten)]
    pub graph: GraphArgs,
}

/// Apply config-file defaults to clap's `Command` before parsing.
///
/// This mutates argument default values so they appear in `--help` and
/// take effect when the user does not pass the corresponding flag.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Config is moved into closures captured by mut_subcommand which requires 'static"
)]
pub fn apply_config_defaults(config: Config, cmd: Command) -> Command {
    // Global args live on the root; every other arg has exactly one home on
    // its own subcommand.
    let cmd = apply_global_defaults(&config, cmd);
    // Clone for the closures that mut_subcommand requires ('static).
    let config2 = config.clone();
    let cmd = cmd.mut_subcommand("submit", |sub| {
        let sub = apply_submit_defaults(&config, sub);
        apply_graph_defaults(&config, sub)
    });
    cmd.mut_subcommand("show", |sub| apply_graph_defaults(&config2, sub))
}

/// Parse the `SubmitArgs` that a bare `stakk` runs with.
///
/// The args come from a real clap parse of the synthetic argv `stakk submit`
/// against a `Command` this function config-applies itself, so clap defaults,
/// `STAKK_*` environment variables and config-injected defaults all apply,
/// exactly as they would for a typed `stakk submit`. Building `SubmitArgs` by
/// hand would bypass all three — which is why `SubmitArgs` has no `Default`
/// impl. Taking the `Config` rather than a prepared `Command` keeps the
/// config application inside this function, where a caller cannot skip it.
pub fn default_submit_args(config: Config) -> Result<SubmitArgs, clap::Error> {
    let cmd = apply_config_defaults(config, Cli::command());
    let matches = cmd.try_get_matches_from(["stakk", "submit"])?;
    let submit = matches
        .subcommand_matches("submit")
        .expect("the synthetic argv always names the submit subcommand");
    SubmitArgs::from_arg_matches(submit)
}

fn set_default(cmd: Command, arg_id: &str, value: &str) -> Command {
    // Leak the value so clap can store it as a `'static` default. This is
    // acceptable because the CLI runs once and exits — the leaked count is
    // bounded by the number of config fields.
    let leaked: &'static str = Box::leak(value.to_string().into_boxed_str());
    cmd.mut_arg(arg_id, |a| a.default_value(leaked))
}

/// Defaults for `global = true` args on the root command.
///
/// Global args are defined once on the root and propagated to subcommands by
/// clap, so `mut_arg` must run on the root — calling it on a subcommand would
/// panic with "Argument is undefined".
fn apply_global_defaults(config: &Config, mut cmd: Command) -> Command {
    if let Some(ref host) = config.github_host {
        cmd = set_default(cmd, "github_host", host);
    }
    cmd
}

fn apply_submit_defaults(config: &Config, mut cmd: Command) -> Command {
    if let Some(ref remote) = config.remote {
        cmd = set_default(cmd, "remote", remote);
    }
    if let Some(pr_mode) = config.pr_mode {
        cmd = set_default(cmd, "pr_mode", &pr_mode.to_string());
    }
    if let Some(ref template_path) = config.template_path {
        cmd = set_default(cmd, "template_path", template_path);
    }
    if let Some(sp) = config.stack_placement {
        cmd = set_default(cmd, "stack_placement", &sp.to_string());
    }
    if let Some(spc) = config.sync_pr_content {
        cmd = set_default(cmd, "sync_pr_content", &spc.to_string());
    }
    if let Some(tr) = config.trailers {
        cmd = set_default(cmd, "trailers", &tr.to_string());
    }
    if let Some(ref ap) = config.auto_prefix {
        cmd = set_default(cmd, "auto_prefix", ap);
    }
    if let Some(ref bc) = config.bookmark_command {
        cmd = set_default(cmd, "bookmark_command", bc);
    }
    cmd
}

fn apply_graph_defaults(config: &Config, mut cmd: Command) -> Command {
    if let Some(ref br) = config.bookmarks_revset {
        cmd = set_default(cmd, "bookmarks_revset", br);
    }
    if let Some(ref hr) = config.heads_revset {
        cmd = set_default(cmd, "heads_revset", hr);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::comment::StackPlacement;

    /// Parse CLI args with the given config applied, returning the `Cli`.
    fn parse_with_config(config: Config, args: &[&str]) -> Cli {
        let cmd = apply_config_defaults(config, Cli::command());
        let matches = cmd.get_matches_from(args);
        Cli::from_arg_matches(&matches).unwrap()
    }

    /// Extract `SubmitArgs` from a parsed `stakk submit` invocation.
    fn submit_args(cli: &Cli) -> &SubmitArgs {
        match &cli.command {
            Some(Commands::Submit(args)) => args,
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    // -- pr_mode tests --

    use crate::cli::submit::PrMode;

    #[test]
    fn pr_mode_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).pr_mode, PrMode::Regular);
    }

    #[test]
    fn pr_mode_config_draft_no_flag() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).pr_mode, PrMode::Draft);
    }

    #[test]
    fn pr_mode_config_regular_no_flag() {
        let config = Config {
            pr_mode: Some(PrMode::Regular),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).pr_mode, PrMode::Regular);
    }

    #[test]
    fn pr_mode_cli_draft() {
        // Config says regular, so this pins CLI-beats-config in the draft
        // direction; pr_mode_config_draft_cli_regular covers the reverse.
        let config = Config {
            pr_mode: Some(PrMode::Regular),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--pr-mode", "draft"]);
        assert_eq!(submit_args(&cli).pr_mode, PrMode::Draft);
    }

    #[test]
    fn pr_mode_cli_overrides_config() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--pr-mode", "regular"]);
        assert_eq!(submit_args(&cli).pr_mode, PrMode::Regular);
    }

    // -- the bare `stakk` form --

    #[test]
    fn bare_stakk_parses_to_no_subcommand() {
        let cli = parse_with_config(Config::default(), &["stakk"]);
        assert!(cli.command.is_none());
    }

    /// Regression guard for 90718ef5cf97 ("fix: respect env vars when running
    /// without subcommand").
    ///
    /// The bare `stakk` form must take its `SubmitArgs` from a clap parse of
    /// the config-applied `Command` — the mechanism `default_submit_args`
    /// implements and `main.rs`'s `None` arm calls. A hand-built value would
    /// silently ignore both `STAKK_*` environment variables and the config
    /// defaults passed here. Env vars ride the same parse, so pinning the
    /// config path pins both; clap's own env handling is not ours to test.
    #[test]
    fn bare_stakk_submit_args_come_from_a_config_applied_clap_parse() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let args = default_submit_args(config).unwrap();
        assert_eq!(args.pr_mode, PrMode::Draft);
    }

    #[test]
    fn bare_stakk_rejects_submit_flags() {
        use clap::error::ErrorKind;

        let cmd = apply_config_defaults(Config::default(), Cli::command());
        let err = cmd
            .try_get_matches_from(["stakk", "--dry-run"])
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    // -- remote tests --

    #[test]
    fn remote_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).remote, "origin");
    }

    #[test]
    fn remote_config_override() {
        let config = Config {
            remote: Some("upstream".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).remote, "upstream");
    }

    #[test]
    fn remote_cli_overrides_config() {
        let config = Config {
            remote: Some("upstream".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--remote", "other"]);
        assert_eq!(submit_args(&cli).remote, "other");
    }

    // -- github_host tests --
    //
    // github_host is a `global = true` arg on the root command, so its config
    // default is injected there rather than per subcommand. These cover that it
    // still reaches every subcommand.

    #[test]
    fn github_host_default_none() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(cli.github_host, None);
    }

    #[test]
    fn github_host_from_config() {
        let config = Config {
            github_host: Some("github.example.com".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(cli.github_host.as_deref(), Some("github.example.com"));
    }

    #[test]
    fn github_host_cli_overrides_config() {
        let config = Config {
            github_host: Some("github.example.com".into()),
            ..Default::default()
        };
        let cli = parse_with_config(
            config,
            &["stakk", "submit", "--github-host", "ghe.other.com"],
        );
        assert_eq!(cli.github_host.as_deref(), Some("ghe.other.com"));
    }

    #[test]
    fn github_host_is_global_and_parses_before_the_subcommand() {
        // `global = true` makes it insertable anywhere; the submit flags are
        // accepted only after `submit`.
        let cli = parse_with_config(
            Config::default(),
            &["stakk", "--github-host", "ghe.example.com", "submit"],
        );
        assert_eq!(cli.github_host.as_deref(), Some("ghe.example.com"));
    }

    #[test]
    fn github_host_from_config_reaches_show() {
        let config = Config {
            github_host: Some("github.example.com".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "show"]);
        assert_eq!(cli.github_host.as_deref(), Some("github.example.com"));
    }

    #[test]
    fn github_host_from_config_reaches_auth_test() {
        let config = Config {
            github_host: Some("github.example.com".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "auth", "test"]);
        assert_eq!(cli.github_host.as_deref(), Some("github.example.com"));
    }

    /// The bare form has no subcommand to carry the global arg, and `main.rs`
    /// reads `github_host` off this parse — not off the synthetic `submit`
    /// parse, which yields a `SubmitArgs` with no host field.
    #[test]
    fn github_host_from_config_reaches_bare_stakk() {
        let config = Config {
            github_host: Some("github.example.com".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk"]);
        assert_eq!(cli.github_host.as_deref(), Some("github.example.com"));
    }

    // -- template_path tests --

    #[test]
    fn template_path_default_none() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).template_path, None);
    }

    #[test]
    fn template_path_config_override() {
        let config = Config {
            template_path: Some("/from/config.jinja".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(
            submit_args(&cli).template_path.as_deref(),
            Some("/from/config.jinja"),
        );
    }

    #[test]
    fn template_path_cli_overrides_config() {
        let config = Config {
            template_path: Some("/from/config.jinja".into()),
            ..Default::default()
        };
        let cli = parse_with_config(
            config,
            &["stakk", "submit", "--template-path", "/from/cli.jinja"],
        );
        assert_eq!(
            submit_args(&cli).template_path.as_deref(),
            Some("/from/cli.jinja"),
        );
    }

    // -- stack_placement tests --

    #[test]
    fn stack_placement_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Comment);
    }

    #[test]
    fn stack_placement_config_body() {
        let config = Config {
            stack_placement: Some(StackPlacement::Body),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Body);
    }

    #[test]
    fn stack_placement_cli_overrides_config() {
        let config = Config {
            stack_placement: Some(StackPlacement::Body),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--stack-placement", "comment"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Comment);
    }

    #[test]
    fn stack_placement_config_none() {
        let config = Config {
            stack_placement: Some(StackPlacement::None),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::None);
    }

    #[test]
    fn stack_placement_cli_none_overrides_config() {
        let config = Config {
            stack_placement: Some(StackPlacement::Body),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--stack-placement", "none"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::None);
    }

    // -- sync_pr_content tests --

    #[test]
    fn sync_pr_content_default_none() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(
            submit_args(&cli).sync_pr_content,
            crate::cli::submit::SyncPrContent::None,
        );
    }

    #[test]
    fn sync_pr_content_config_all() {
        let config = Config {
            sync_pr_content: Some(crate::cli::submit::SyncPrContent::All),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(
            submit_args(&cli).sync_pr_content,
            crate::cli::submit::SyncPrContent::All,
        );
    }

    #[test]
    fn sync_pr_content_cli_overrides_config() {
        let config = Config {
            sync_pr_content: Some(crate::cli::submit::SyncPrContent::All),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--sync-pr-content=title"]);
        assert_eq!(
            submit_args(&cli).sync_pr_content,
            crate::cli::submit::SyncPrContent::Title,
        );
    }

    // -- trailers tests --

    #[test]
    fn trailers_default_keep() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit"]);
        assert_eq!(
            submit_args(&cli).trailers,
            crate::cli::submit::TrailerHandling::Keep,
        );
    }

    #[test]
    fn trailers_config_strip() {
        let config = Config {
            trailers: Some(crate::cli::submit::TrailerHandling::Strip),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(
            submit_args(&cli).trailers,
            crate::cli::submit::TrailerHandling::Strip,
        );
    }

    #[test]
    fn trailers_cli_overrides_config() {
        let config = Config {
            trailers: Some(crate::cli::submit::TrailerHandling::Strip),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--trailers=keep"]);
        assert_eq!(
            submit_args(&cli).trailers,
            crate::cli::submit::TrailerHandling::Keep,
        );
    }

    // -- auto_prefix tests --

    #[test]
    fn auto_prefix_config_override() {
        let config = Config {
            auto_prefix: Some("gb-".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).auto_prefix.as_deref(), Some("gb-"));
    }

    #[test]
    fn auto_prefix_cli_overrides_config() {
        let config = Config {
            auto_prefix: Some("gb-".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--auto-prefix", "xx-"]);
        assert_eq!(submit_args(&cli).auto_prefix.as_deref(), Some("xx-"));
    }

    // -- graph revset tests --

    #[test]
    fn bookmarks_revset_config_override() {
        let config = Config {
            bookmarks_revset: Some("all()".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).graph.bookmarks_revset, "all()");
    }

    #[test]
    fn heads_revset_config_override() {
        let config = Config {
            heads_revset: Some("heads(all())".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit"]);
        assert_eq!(submit_args(&cli).graph.heads_revset, "heads(all())");
    }

    #[test]
    fn revset_cli_overrides_config() {
        let config = Config {
            bookmarks_revset: Some("all()".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--bookmarks-revset", "mine()"]);
        assert_eq!(submit_args(&cli).graph.bookmarks_revset, "mine()");
    }

    // -- show subcommand gets graph defaults --

    #[test]
    fn show_inherits_graph_defaults() {
        let config = Config {
            bookmarks_revset: Some("custom()".into()),
            heads_revset: Some("heads(custom())".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "show"]);
        match &cli.command {
            Some(Commands::Show(args)) => {
                assert_eq!(args.graph.bookmarks_revset, "custom()");
                assert_eq!(args.graph.heads_revset, "heads(custom())");
            }
            other => panic!("expected Show, got {other:?}"),
        }
    }

    // -- docs subcommand --

    #[test]
    fn docs_without_topic_is_none() {
        // `None` means "print the index", not "print a default topic".
        let cli = parse_with_config(Config::default(), &["stakk", "docs"]);
        match &cli.command {
            Some(Commands::Docs { topic }) => assert_eq!(*topic, None),
            other => panic!("expected Docs, got {other:?}"),
        }
    }

    #[test]
    fn docs_parses_each_topic() {
        for (arg, expected) in [
            ("scripting", DocTopic::Scripting),
            ("config", DocTopic::Config),
            ("template", DocTopic::Template),
        ] {
            let cli = parse_with_config(Config::default(), &["stakk", "docs", arg]);
            match &cli.command {
                Some(Commands::Docs { topic }) => assert_eq!(*topic, Some(expected)),
                other => panic!("expected Docs, got {other:?}"),
            }
        }
    }

    #[test]
    fn docs_rejects_an_unknown_topic() {
        use clap::error::ErrorKind;

        let cmd = apply_config_defaults(Config::default(), Cli::command());
        let err = cmd
            .try_get_matches_from(["stakk", "docs", "nonsense"])
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    // -- explicit selection flags --

    #[test]
    fn selection_flags_parse_and_accumulate() {
        let cli = parse_with_config(
            Config::default(),
            &[
                "stakk",
                "submit",
                "--keep",
                "a",
                "--keep",
                "b",
                "--new",
                "r1=name1",
                "--new",
                "r2",
                "--new-auto",
                "r3",
                "--new-command",
                "r4",
            ],
        );
        let args = submit_args(&cli);
        assert_eq!(args.keep, vec!["a", "b"]);
        assert_eq!(args.new, vec!["r1=name1", "r2"]);
        assert_eq!(args.new_auto, vec!["r3"]);
        assert_eq!(args.new_command, vec!["r4"]);
    }

    // -- env var interaction --

    #[test]
    fn env_var_overrides_config() {
        // env vars are set per-process, so this test just verifies the
        // precedence: CLI > env > config > hardcoded default.
        // We can't easily test env vars in unit tests without side effects,
        // so this test documents the expected clap precedence.
        let config = Config {
            remote: Some("from-config".into()),
            ..Default::default()
        };
        // CLI flag should override config.
        let cli = parse_with_config(config, &["stakk", "submit", "--remote", "from-cli"]);
        assert_eq!(submit_args(&cli).remote, "from-cli");
    }

    // -- TOML parsing --

    #[test]
    fn toml_deserialize_full() {
        let toml_str = r#"
remote = "upstream"
github_host = "github.example.com"
pr_mode = "draft"
template_path = "/path/to/template.jinja"
stack_placement = "body"
sync_pr_content = "all"
trailers = "strip"
auto_prefix = "gb-"
bookmark_command = "my-command"
bookmarks_revset = "all()"
heads_revset = "heads(all())"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remote.as_deref(), Some("upstream"));
        assert_eq!(config.github_host.as_deref(), Some("github.example.com"));
        assert_eq!(config.pr_mode, Some(PrMode::Draft));
        assert_eq!(
            config.template_path.as_deref(),
            Some("/path/to/template.jinja"),
        );
        assert_eq!(config.stack_placement, Some(StackPlacement::Body));
        assert_eq!(
            config.sync_pr_content,
            Some(crate::cli::submit::SyncPrContent::All),
        );
        assert_eq!(
            config.trailers,
            Some(crate::cli::submit::TrailerHandling::Strip),
        );
        assert_eq!(config.auto_prefix.as_deref(), Some("gb-"));
        assert_eq!(config.bookmark_command.as_deref(), Some("my-command"));
        assert_eq!(config.bookmarks_revset.as_deref(), Some("all()"));
        assert_eq!(config.heads_revset.as_deref(), Some("heads(all())"));
    }

    #[test]
    fn toml_deserialize_empty() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.remote.is_none());
        assert!(config.pr_mode.is_none());
    }

    #[test]
    fn toml_deserialize_partial() {
        let config: Config = toml::from_str(r#"pr_mode = "regular""#).unwrap();
        assert_eq!(config.pr_mode, Some(PrMode::Regular));
        assert!(config.remote.is_none());
    }

    #[test]
    fn toml_rejects_unknown_field() {
        let result: Result<Config, _> = toml::from_str("bogus = 42");
        assert!(result.is_err());
    }

    #[test]
    fn toml_stack_placement_kebab_case() {
        let config: Config = toml::from_str(r#"stack_placement = "comment""#).unwrap();
        assert_eq!(config.stack_placement, Some(StackPlacement::Comment));
    }

    #[test]
    fn toml_stack_placement_none() {
        let config: Config = toml::from_str(r#"stack_placement = "none""#).unwrap();
        assert_eq!(config.stack_placement, Some(StackPlacement::None));
    }

    #[test]
    fn toml_stack_placement_ignore() {
        let config: Config = toml::from_str(r#"stack_placement = "ignore""#).unwrap();
        assert_eq!(config.stack_placement, Some(StackPlacement::Ignore));
    }

    #[test]
    fn toml_stack_placement_invalid() {
        let result: Result<Config, _> = toml::from_str(r#"stack_placement = "invalid""#);
        assert!(result.is_err());
    }
}
