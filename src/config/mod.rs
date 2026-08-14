use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::cli::submit::PrMode;
use crate::cli::submit::SyncPrContent;
use crate::cli::submit::TrailerHandling;
use crate::forge::comment::StackPlacement;

/// Pre-parse the config file path from raw CLI args or environment, before clap
/// runs.
///
/// We need the config path *before* clap parsing because config values are
/// injected as clap defaults (so they appear in `--help` and respect the
/// CLI > env > config precedence). Clap can't parse the args until those
/// defaults are set, so we scan `std::env::args()` directly.
///
/// The corresponding `--config` clap arg in `Cli` exists purely for help text
/// and discoverability — its parsed value is never read.
///
/// Precedence: `--config <path>` flag > `STAKK_CONFIG` env var > automatic
/// discovery.
pub fn pre_parse_config_path() -> Option<PathBuf> {
    // Scan raw args for `--config <path>` or `--config=<path>`.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            if let Some(value) = args.next() {
                return Some(PathBuf::from(value));
            }
        } else if let Some(value) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(value));
        }
    }

    // Fall back to the environment variable.
    std::env::var("STAKK_CONFIG").ok().map(PathBuf::from)
}

/// An environment variable stakk no longer reads, paired with the advice that
/// replaces it.
#[derive(Debug, PartialEq, Eq)]
pub struct RemovedEnvVar {
    pub name: &'static str,
    /// Imperative advice, rendered after "`<name>` is no longer read; ".
    pub advice: &'static str,
}

/// The `STAKK_*` environment variables stakk used to read and no longer does.
///
/// Removed *flags* need no entry: clap rejects an unknown argument. Removed
/// *config keys* need none either: `Config` denies unknown fields and lists the
/// valid ones. The environment is the only configuration surface where a stale
/// setting is silently ignored, which is why this table exists.
///
/// **Removal:** this table, [`removed_env_vars`] and its caller in `main.rs` go
/// at v3.0.0 at the latest, and may go in any 2.x once the 1.x population has
/// moved. Advisory warnings are explicitly not stable surface (see
/// `docs/stability.md`), so deleting them is not a break.
static REMOVED_ENV_VARS: &[RemovedEnvVar] = &[
    RemovedEnvVar {
        name: "STAKK_DRAFT",
        advice: "use STAKK_PR_MODE=draft or pr_mode in stakk.toml",
    },
    RemovedEnvVar {
        name: "STAKK_TEMPLATE",
        advice: "use STAKK_TEMPLATE_PATH or template_path in stakk.toml",
    },
];

/// The removed environment variables that are currently set to a non-empty
/// value.
///
/// `lookup` is injected so tests do not have to mutate the process
/// environment, the way `auth::token_from_env` takes its lookup.
///
/// An empty value counts as absent — the same rule `main.rs` applies to
/// `GH_HOST` — so `STAKK_DRAFT=` is not reported, and emptying the variable is
/// a way to silence the warning where the export cannot be dropped.
pub fn removed_env_vars(lookup: impl Fn(&str) -> Option<String>) -> Vec<&'static RemovedEnvVar> {
    REMOVED_ENV_VARS
        .iter()
        .filter(|removed| lookup(removed.name).is_some_and(|value| !value.is_empty()))
        .collect()
}

/// Print one stderr warning per removed environment variable that is still set.
///
/// Advisory only: the variable may well belong to something other than stakk,
/// so this never fails the command.
pub fn warn_removed_env_vars() {
    for removed in removed_env_vars(|name| std::env::var(name).ok()) {
        eprintln!(
            "Warning: {} is no longer read; {}",
            removed.name, removed.advice
        );
    }
}

/// Persistent configuration loaded from a config file.
///
/// All fields are optional — absent fields fall back to CLI defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// When `false` in a repo-level `stakk.toml`, the user config is skipped.
    #[serde(default = "default_true")]
    pub inherit: bool,
    pub remote: Option<String>,
    /// Extra host to treat as GitHub, for GitHub Enterprise Server.
    pub github_host: Option<String>,
    pub pr_mode: Option<PrMode>,
    pub template_path: Option<String>,
    pub stack_placement: Option<StackPlacement>,
    pub sync_pr_content: Option<SyncPrContent>,
    pub trailers: Option<TrailerHandling>,
    pub auto_prefix: Option<String>,
    pub bookmark_command: Option<String>,
    pub bookmarks_revset: Option<String>,
    pub heads_revset: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            inherit: true,
            remote: None,
            github_host: None,
            pr_mode: None,
            template_path: None,
            stack_placement: None,
            sync_pr_content: None,
            trailers: None,
            auto_prefix: None,
            bookmark_command: None,
            bookmarks_revset: None,
            heads_revset: None,
        }
    }
}

fn default_true() -> bool {
    true
}

impl Config {
    /// Load config from a TOML file, returning `Default` if the file does not
    /// exist.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(ConfigError::ReadFailed {
                    path: path.display().to_string(),
                    source: e,
                });
            }
        };
        toml::from_str(&contents).map_err(|source| ConfigError::ParseFailed {
            path: path.display().to_string(),
            source,
        })
    }

    /// Discover and load config, merging repo-level and user-level config
    /// according to precedence rules.
    ///
    /// If `explicit_path` is `Some`, it replaces the automatic
    /// `discover_repo_config()` walk. The user-level config is still merged
    /// unless the loaded config sets `inherit = false`.
    pub fn load(explicit_path: Option<PathBuf>) -> Result<Self, ConfigError> {
        let repo_config = match explicit_path.or_else(discover_repo_config) {
            Some(path) => Self::load_from(&path)?,
            None => Self::default(),
        };

        if !repo_config.inherit {
            return Ok(repo_config);
        }

        let user_config = match user_config_path() {
            Some(path) => Self::load_from(&path)?,
            None => Self::default(),
        };

        Ok(repo_config.merge(user_config))
    }

    /// Merge `self` with a fallback config. For each `Option` field, `self`
    /// wins if `Some`, otherwise `fallback` is used. `inherit` is not
    /// merged — it is a directive, not a setting.
    fn merge(self, fallback: Self) -> Self {
        Self {
            inherit: self.inherit,
            remote: self.remote.or(fallback.remote),
            github_host: self.github_host.or(fallback.github_host),
            pr_mode: self.pr_mode.or(fallback.pr_mode),
            template_path: self.template_path.or(fallback.template_path),
            stack_placement: self.stack_placement.or(fallback.stack_placement),
            sync_pr_content: self.sync_pr_content.or(fallback.sync_pr_content),
            trailers: self.trailers.or(fallback.trailers),
            auto_prefix: self.auto_prefix.or(fallback.auto_prefix),
            bookmark_command: self.bookmark_command.or(fallback.bookmark_command),
            bookmarks_revset: self.bookmarks_revset.or(fallback.bookmarks_revset),
            heads_revset: self.heads_revset.or(fallback.heads_revset),
        }
    }
}

/// Walk from cwd upward, returning the first `stakk.toml` found.
///
/// Stops at the jj workspace root (the directory containing `.jj/`) to avoid
/// picking up unrelated config files from parent directories. Since stakk is
/// a jj tool, `.jj/` is the natural repo boundary — it exists in both
/// colocated and non-colocated jj repos.
fn discover_repo_config() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("stakk.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        // Stop at the jj workspace root — don't look beyond the repo.
        if dir.join(".jj").is_dir() {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Return the user-level config path:
/// `{config_dir}/stakk/config.toml`
fn user_config_path() -> Option<PathBuf> {
    // Empty qualifier and organization — we want `stakk/config.toml` directly
    // under the platform config dir, not a reverse-domain-style path.
    let proj = directories::ProjectDirs::from("", "", "stakk")?;
    Some(proj.config_dir().join("config.toml"))
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum ConfigError {
    #[error("failed to read config file {path}")]
    #[diagnostic(help("check file permissions"))]
    ReadFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config file {path}")]
    #[diagnostic(help("check the TOML syntax and field names"))]
    ParseFailed {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_self_wins() {
        let a = Config {
            remote: Some("from-repo".into()),
            pr_mode: Some(PrMode::Draft),
            ..Default::default()
        };
        let b = Config {
            remote: Some("from-user".into()),
            pr_mode: Some(PrMode::Regular),
            template_path: Some("user-template".into()),
            ..Default::default()
        };
        let merged = a.merge(b);
        assert_eq!(merged.remote.as_deref(), Some("from-repo"));
        assert_eq!(merged.pr_mode, Some(PrMode::Draft));
        assert_eq!(merged.template_path.as_deref(), Some("user-template"));
    }

    #[test]
    fn merge_fallback_fills_gaps() {
        let a = Config::default();
        let b = Config {
            remote: Some("from-user".into()),
            ..Default::default()
        };
        let merged = a.merge(b);
        assert_eq!(merged.remote.as_deref(), Some("from-user"));
    }

    #[test]
    fn inherit_defaults_to_true() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.inherit);
    }

    #[test]
    fn inherit_false_in_toml() {
        let config: Config = toml::from_str("inherit = false").unwrap();
        assert!(!config.inherit);
    }

    #[test]
    fn load_from_nonexistent_returns_default() {
        let config = Config::load_from(Path::new("/nonexistent/stakk.toml")).unwrap();
        assert!(config.remote.is_none());
        assert!(config.inherit);
    }

    /// The lookup closure the tests inject: a fixed table, so the process
    /// environment is never read or written.
    fn lookup_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn matched(vars: &[&RemovedEnvVar]) -> Vec<(&'static str, &'static str)> {
        vars.iter().map(|v| (v.name, v.advice)).collect()
    }

    #[test]
    fn removed_env_vars_none_set() {
        assert!(removed_env_vars(lookup_from(&[])).is_empty());
    }

    #[test]
    fn removed_env_vars_reports_draft_and_template() {
        let found = removed_env_vars(lookup_from(&[
            ("STAKK_DRAFT", "1"),
            ("STAKK_TEMPLATE", "/path/to/template.md.jinja"),
        ]));
        assert_eq!(
            matched(&found),
            vec![
                (
                    "STAKK_DRAFT",
                    "use STAKK_PR_MODE=draft or pr_mode in stakk.toml"
                ),
                (
                    "STAKK_TEMPLATE",
                    "use STAKK_TEMPLATE_PATH or template_path in stakk.toml"
                ),
            ]
        );
    }

    #[test]
    fn removed_env_vars_treats_empty_as_unset() {
        let found = removed_env_vars(lookup_from(&[("STAKK_DRAFT", ""), ("STAKK_TEMPLATE", "")]));
        assert!(matched(&found).is_empty());
    }

    #[test]
    fn removed_env_vars_ignores_current_names() {
        // Names are matched exactly: STAKK_TEMPLATE_PATH is not STAKK_TEMPLATE.
        let found = removed_env_vars(lookup_from(&[
            ("STAKK_PR_MODE", "draft"),
            ("STAKK_TEMPLATE_PATH", "/path/to/template.md.jinja"),
        ]));
        assert!(matched(&found).is_empty());
    }

    #[test]
    fn user_config_path_is_some() {
        // On most systems, ProjectDirs should succeed.
        let path = user_config_path();
        if let Some(p) = path {
            assert!(p.ends_with("config.toml"));
        }
    }
}
