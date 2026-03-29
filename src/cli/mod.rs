pub mod auth;
pub mod graph;
pub mod submit;

use std::path::PathBuf;

use clap::Args;
use clap::Command;
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
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// Path to a config file (overrides automatic discovery).
    ///
    /// The file is loaded in place of the repo-level stakk.toml;
    /// user-level config is still merged unless inherit = false.
    // Implementation note: this arg exists for --help discoverability only.
    // Config is loaded *before* clap parsing (so config values can be injected
    // as clap defaults), which means clap's parsed value arrives too late.
    // The actual path is resolved by `config::pre_parse_config_path()` from
    // raw `std::env::args()` / `STAKK_CONFIG`.
    #[arg(long, global = true, env = "STAKK_CONFIG", verbatim_doc_comment)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
    /// Default submit arguments (used when no subcommand is given).
    #[command(flatten)]
    pub submit_args: SubmitArgs,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Submit bookmarks as GitHub pull requests (default when no command
    /// given).
    Submit(SubmitArgs),
    /// Manage authentication.
    Auth(AuthArgs),
    /// Show repository status and bookmark stacks.
    Show(ShowArgs),
    /// Generate shell completions for the given shell.
    Completions {
        /// The shell to generate completions for.
        shell: Shell,
    },
}

/// Arguments for the show subcommand.
#[derive(Debug, Args)]
pub struct ShowArgs {
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
    // Apply to top-level (flattened submit args) first.
    let cmd = apply_submit_and_graph_defaults(&config, cmd);
    // Clone for the closures that mut_subcommand requires ('static).
    let config2 = config.clone();
    let cmd = cmd.mut_subcommand("submit", |sub| {
        apply_submit_and_graph_defaults(&config, sub)
    });
    cmd.mut_subcommand("show", |sub| apply_graph_defaults(&config2, sub))
}

fn set_default(cmd: Command, arg_id: &str, value: &str) -> Command {
    // Leak the value so clap can store it as a `'static` default. This is
    // acceptable because the CLI runs once and exits — the leaked count is
    // bounded by the number of config fields.
    let leaked: &'static str = Box::leak(value.to_string().into_boxed_str());
    cmd.mut_arg(arg_id, |a| a.default_value(leaked))
}

fn apply_submit_defaults(config: &Config, mut cmd: Command) -> Command {
    if let Some(ref remote) = config.remote {
        cmd = set_default(cmd, "remote", remote);
    }
    if let Some(pr_mode) = config.pr_mode {
        cmd = set_default(cmd, "pr_mode", &pr_mode.to_string());
    }
    if let Some(ref template) = config.template {
        cmd = set_default(cmd, "template", template);
    }
    if let Some(sp) = config.stack_placement {
        cmd = set_default(cmd, "stack_placement", &sp.to_string());
    }
    if let Some(spc) = config.sync_pr_content {
        cmd = set_default(cmd, "sync_pr_content", &spc.to_string());
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

fn apply_submit_and_graph_defaults(config: &Config, cmd: Command) -> Command {
    let cmd = apply_submit_defaults(config, cmd);
    apply_graph_defaults(config, cmd)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap::FromArgMatches;

    use super::*;
    use crate::forge::comment::StackPlacement;

    /// Parse CLI args with the given config applied, returning the `Cli`.
    fn parse_with_config(config: Config, args: &[&str]) -> Cli {
        let cmd = apply_config_defaults(config, Cli::command());
        let matches = cmd.get_matches_from(args);
        Cli::from_arg_matches(&matches).unwrap()
    }

    /// Extract `SubmitArgs` from parsed CLI (handles both top-level and
    /// subcommand).
    fn submit_args(cli: &Cli) -> &SubmitArgs {
        match &cli.command {
            Some(Commands::Submit(args)) => args,
            _ => &cli.submit_args,
        }
    }

    // -- pr_mode tests --

    use crate::cli::submit::PrMode;

    #[test]
    fn pr_mode_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Regular);
    }

    #[test]
    fn pr_mode_config_draft_no_flag() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    #[test]
    fn pr_mode_config_regular_no_flag() {
        let config = Config {
            pr_mode: Some(PrMode::Regular),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Regular);
    }

    #[test]
    fn pr_mode_config_regular_cli_draft() {
        let config = Config {
            pr_mode: Some(PrMode::Regular),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--draft", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    #[test]
    fn pr_mode_cli_overrides_config() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--pr-mode", "regular", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Regular);
    }

    #[test]
    fn pr_mode_no_config_cli_draft_flag() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit", "--draft", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    #[test]
    fn pr_mode_draft_flag_overrides_pr_mode_regular() {
        let cli = parse_with_config(
            Config::default(),
            &["stakk", "submit", "--pr-mode", "regular", "--draft", "bm"],
        );
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    // -- pr_mode top-level (no subcommand) --

    #[test]
    fn pr_mode_toplevel_config_draft() {
        let config = Config {
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    #[test]
    fn pr_mode_toplevel_cli_draft_flag() {
        let cli = parse_with_config(Config::default(), &["stakk", "--draft", "bm"]);
        assert_eq!(submit_args(&cli).pr_mode(), PrMode::Draft);
    }

    // -- remote tests --

    #[test]
    fn remote_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).remote, "origin");
    }

    #[test]
    fn remote_config_override() {
        let config = Config {
            remote: Some("upstream".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).remote, "upstream");
    }

    #[test]
    fn remote_cli_overrides_config() {
        let config = Config {
            remote: Some("upstream".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--remote", "other", "bm"]);
        assert_eq!(submit_args(&cli).remote, "other");
    }

    // -- stack_placement tests --

    #[test]
    fn stack_placement_default_no_config() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Comment);
    }

    #[test]
    fn stack_placement_config_body() {
        let config = Config {
            stack_placement: Some(StackPlacement::Body),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Body);
    }

    #[test]
    fn stack_placement_cli_overrides_config() {
        let config = Config {
            stack_placement: Some(StackPlacement::Body),
            ..Default::default()
        };
        let cli = parse_with_config(
            config,
            &["stakk", "submit", "--stack-placement", "comment", "bm"],
        );
        assert_eq!(submit_args(&cli).stack_placement, StackPlacement::Comment);
    }

    // -- sync_pr_content tests --

    #[test]
    fn sync_pr_content_default_false() {
        let cli = parse_with_config(Config::default(), &["stakk", "submit", "bm"]);
        assert!(!submit_args(&cli).sync_pr_content);
    }

    #[test]
    fn sync_pr_content_config_true() {
        let config = Config {
            sync_pr_content: Some(true),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert!(submit_args(&cli).sync_pr_content);
    }

    #[test]
    fn sync_pr_content_cli_overrides_config() {
        let config = Config {
            sync_pr_content: Some(true),
            ..Default::default()
        };
        let cli = parse_with_config(
            config,
            &["stakk", "submit", "--sync-pr-content=false", "bm"],
        );
        assert!(!submit_args(&cli).sync_pr_content);
    }

    // -- auto_prefix tests --

    #[test]
    fn auto_prefix_config_override() {
        let config = Config {
            auto_prefix: Some("gb-".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).auto_prefix.as_deref(), Some("gb-"));
    }

    #[test]
    fn auto_prefix_cli_overrides_config() {
        let config = Config {
            auto_prefix: Some("gb-".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "--auto-prefix", "xx-", "bm"]);
        assert_eq!(submit_args(&cli).auto_prefix.as_deref(), Some("xx-"));
    }

    // -- graph revset tests --

    #[test]
    fn bookmarks_revset_config_override() {
        let config = Config {
            bookmarks_revset: Some("all()".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).graph.bookmarks_revset, "all()");
    }

    #[test]
    fn heads_revset_config_override() {
        let config = Config {
            heads_revset: Some("heads(all())".into()),
            ..Default::default()
        };
        let cli = parse_with_config(config, &["stakk", "submit", "bm"]);
        assert_eq!(submit_args(&cli).graph.heads_revset, "heads(all())");
    }

    #[test]
    fn revset_cli_overrides_config() {
        let config = Config {
            bookmarks_revset: Some("all()".into()),
            ..Default::default()
        };
        let cli = parse_with_config(
            config,
            &["stakk", "submit", "--bookmarks-revset", "mine()", "bm"],
        );
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
        let cli = parse_with_config(config, &["stakk", "submit", "--remote", "from-cli", "bm"]);
        assert_eq!(submit_args(&cli).remote, "from-cli");
    }

    // -- TOML parsing --

    #[test]
    fn toml_deserialize_full() {
        let toml_str = r#"
remote = "upstream"
pr_mode = "draft"
template = "/path/to/template.jinja"
stack_placement = "body"
sync_pr_content = true
auto_prefix = "gb-"
bookmark_command = "my-command"
bookmarks_revset = "all()"
heads_revset = "heads(all())"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remote.as_deref(), Some("upstream"));
        assert_eq!(config.pr_mode, Some(PrMode::Draft));
        assert_eq!(config.template.as_deref(), Some("/path/to/template.jinja"));
        assert_eq!(config.stack_placement, Some(StackPlacement::Body));
        assert_eq!(config.sync_pr_content, Some(true));
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
    fn toml_stack_placement_invalid() {
        let result: Result<Config, _> = toml::from_str(r#"stack_placement = "invalid""#);
        assert!(result.is_err());
    }
}
