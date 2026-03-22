use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::cli::submit::PrMode;
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
    pub pr_mode: Option<PrMode>,
    pub template: Option<String>,
    pub stack_placement: Option<StackPlacement>,
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
            pr_mode: None,
            template: None,
            stack_placement: None,
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
            pr_mode: self.pr_mode.or(fallback.pr_mode),
            template: self.template.or(fallback.template),
            stack_placement: self.stack_placement.or(fallback.stack_placement),
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
            template: Some("user-template".into()),
            ..Default::default()
        };
        let merged = a.merge(b);
        assert_eq!(merged.remote.as_deref(), Some("from-repo"));
        assert_eq!(merged.pr_mode, Some(PrMode::Draft));
        assert_eq!(merged.template.as_deref(), Some("user-template"));
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

    #[test]
    fn user_config_path_is_some() {
        // On most systems, ProjectDirs should succeed.
        let path = user_config_path();
        if let Some(p) = path {
            assert!(p.ends_with("config.toml"));
        }
    }
}
