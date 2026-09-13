//! Forge authentication token resolution.
//!
//! Two forges, two resolutions that share nothing but [`AuthToken`]:
//!
//! - **GitHub** ([`resolve_token`]) delegates to the GitHub CLI first, then
//!   falls back to the host's token environment variables.
//! - **Forgejo** ([`resolve_forgejo_token`]) reads `FORGEJO_TOKEN` and nothing
//!   else — no `gh`, no GitHub variables, no per-host split.
//!
//! # GitHub
//!
//! Resolves a token for a specific host by delegating to the GitHub CLI first:
//! 1. `gh auth token --hostname <host>`. Note that gh reads the token
//!    environment variables itself and hands their value back, so an exported
//!    token wins over gh's stored credential without this module ever seeing
//!    the variable.
//! 2. the host's environment variables — `GH_TOKEN`/`GITHUB_TOKEN` for
//!    github.com, `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` for a GitHub
//!    Enterprise Server host. This is the fallback for when gh is absent or has
//!    no token for the host, not a lower-priority alternative to it.
//!
//! The env-var split mirrors the GitHub CLI, so an enterprise token is never
//! sent to github.com and vice versa.

use miette::Diagnostic;
use thiserror::Error;

use crate::GITHUB_COM;

/// How the token was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// From `gh auth token`.
    GitHubCli,
    /// From `GITHUB_TOKEN` environment variable.
    GitHubTokenEnv,
    /// From `GH_TOKEN` environment variable.
    GhTokenEnv,
    /// From `GH_ENTERPRISE_TOKEN` environment variable.
    GhEnterpriseTokenEnv,
    /// From `GITHUB_ENTERPRISE_TOKEN` environment variable.
    GitHubEnterpriseTokenEnv,
    /// From `FORGEJO_TOKEN` environment variable.
    ForgejoTokenEnv,
}

/// A resolved authentication token with its source.
#[derive(Debug, Clone)]
pub struct AuthToken {
    pub token: String,
    /// Which source the token came from. Nothing in the binary reads it —
    /// submit needs only the token itself — but it is what the resolution
    /// tests assert on, and so what pins the per-host precedence order.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the resolution tests read it to pin that gh's answer beats the environment, \
                      and which token environment variable wins for a given host"
        )
    )]
    pub source: TokenSource,
}

/// What a `gh` invocation returned: whether it exited zero, and what it
/// wrote to stdout. Nothing else about the process is consulted.
struct GhOutput {
    success: bool,
    stdout: String,
}

/// Trait for running `gh` commands, mirroring [`crate::jj::runner::JjRunner`].
///
/// The seam is at the argv, not at "get a token for this host", because the
/// argv is the part that has to be right: `gh auth token` without
/// `--hostname` answers for a different host than the one asked about, and a
/// trait that builds the argv internally cannot be observed doing so.
trait GhRunner: Send + Sync {
    fn run_gh(
        &self,
        args: &[&str],
    ) -> impl std::future::Future<Output = Result<GhOutput, std::io::Error>> + Send;
}

/// Runs `gh` commands via `tokio::process::Command`.
struct RealGhRunner;

impl GhRunner for RealGhRunner {
    async fn run_gh(&self, args: &[&str]) -> Result<GhOutput, std::io::Error> {
        let output = tokio::process::Command::new("gh")
            .args(args)
            .output()
            .await?;
        Ok(GhOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        })
    }
}

/// Errors from authentication resolution.
#[derive(Debug, Error, Diagnostic)]
pub enum AuthError {
    #[error("no GitHub authentication found for {host}")]
    #[diagnostic(
        code(stakk::auth::no_token),
        help("run `gh auth login --hostname {host}`, or set {}", env_var_list(host))
    )]
    NoAuthFound { host: String },

    #[error("no Forgejo token found for {host}")]
    #[diagnostic(
        code(stakk::auth::no_forgejo_token),
        help(
            "set {FORGEJO_TOKEN} to a personal access token — Forgejo mints them under Settings → \
             Applications on {host}"
        )
    )]
    NoForgejoToken { host: String },

    #[error("failed to run `gh auth token`: {0}")]
    #[diagnostic(
        code(stakk::auth::gh_cli_error),
        help(
            "repair the `gh` installation — a `gh` that cannot be started stops resolution before \
             the token environment variables are read"
        )
    )]
    GhCliError(std::io::Error),
}

/// The token environment variables that apply to `host`, most preferred first.
///
/// Both pairs follow the order `gh help environment` documents, so this
/// fallback and gh's own answer resolve to the same token — which one applies
/// does not depend on whether gh happens to be installed.
fn env_sources(host: &str) -> &'static [(&'static str, TokenSource)] {
    if host == GITHUB_COM {
        &[
            ("GH_TOKEN", TokenSource::GhTokenEnv),
            ("GITHUB_TOKEN", TokenSource::GitHubTokenEnv),
        ]
    } else {
        &[
            ("GH_ENTERPRISE_TOKEN", TokenSource::GhEnterpriseTokenEnv),
            (
                "GITHUB_ENTERPRISE_TOKEN",
                TokenSource::GitHubEnterpriseTokenEnv,
            ),
        ]
    }
}

/// The host's environment variable names, formatted for a help message.
fn env_var_list(host: &str) -> String {
    let names: Vec<&str> = env_sources(host).iter().map(|(name, _)| *name).collect();
    names.join("/")
}

/// Look up the first non-empty token among the host's environment variables.
///
/// `lookup` is injected so tests do not have to mutate the process
/// environment.
fn token_from_env(host: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<AuthToken> {
    env_sources(host).iter().find_map(|(name, source)| {
        lookup(name)
            .filter(|token| !token.is_empty())
            .map(|token| AuthToken {
                token,
                source: *source,
            })
    })
}

/// Resolve a GitHub authentication token for `host`.
///
/// Asks the gh CLI first — which answers from the environment itself when a
/// token for the host is set — and reads the host's environment variables here
/// only when gh is absent or has nothing for the host. Returns the first token
/// found, or `AuthError::NoAuthFound`.
///
/// This does NOT validate the token against the GitHub API: an expired or
/// revoked token resolves fine and fails at the first API call.
pub async fn resolve_token(host: &str) -> Result<AuthToken, AuthError> {
    resolve_token_with(host, &RealGhRunner, |name| std::env::var(name).ok()).await
}

/// [`resolve_token`] with the `gh` invocation and the environment lookup
/// injected, so the precedence between them is testable without a `gh`
/// binary or a mutated process environment.
async fn resolve_token_with(
    host: &str,
    gh: &impl GhRunner,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<AuthToken, AuthError> {
    if let Some(token) = try_gh_cli(host, gh).await? {
        return Ok(AuthToken {
            token,
            source: TokenSource::GitHubCli,
        });
    }

    token_from_env(host, lookup).ok_or_else(|| AuthError::NoAuthFound {
        host: host.to_string(),
    })
}

/// Try to get a token from the GitHub CLI for `host`.
///
/// `--hostname` is passed explicitly: without it gh answers for whichever host
/// `GH_HOST` or its config names, which need not be the host this repo's
/// remote points at.
///
/// Returns `Ok(None)` if gh is not installed or not authenticated for the host.
/// Returns `Err` only for unexpected I/O failures.
async fn try_gh_cli(host: &str, gh: &impl GhRunner) -> Result<Option<String>, AuthError> {
    let result = gh.run_gh(&["auth", "token", "--hostname", host]).await;

    match result {
        Ok(output) if output.success => {
            let token = output.stdout.trim().to_string();
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(token))
            }
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AuthError::GhCliError(e)),
    }
}

/// The one environment variable a Forgejo token is read from.
const FORGEJO_TOKEN: &str = "FORGEJO_TOKEN";

/// Resolve a Forgejo authentication token for `host`.
///
/// Reads `FORGEJO_TOKEN` and nothing else: no `gh` (it knows nothing about
/// Forgejo), none of the GitHub variables (a GitHub token must never be sent
/// to a Forgejo host), and no per-host split — there is no "enterprise token
/// must never reach the public host" rule to mirror, so a user with two
/// Forgejo hosts sets the variable per shell, as for any CLI. `host` only
/// names the instance in the error.
///
/// Like [`resolve_token`], this does NOT validate the token: a revoked one
/// resolves fine and fails at the first API call.
pub fn resolve_forgejo_token(host: &str) -> Result<AuthToken, AuthError> {
    resolve_forgejo_token_with(host, |name| std::env::var(name).ok())
}

/// [`resolve_forgejo_token`] with the environment lookup injected, so tests
/// never mutate the process environment. There is no runner to inject: the
/// signature has nowhere to run `gh` from, which is the guarantee.
fn resolve_forgejo_token_with(
    host: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<AuthToken, AuthError> {
    lookup(FORGEJO_TOKEN)
        .filter(|token| !token.is_empty())
        .map(|token| AuthToken {
            token,
            source: TokenSource::ForgejoTokenEnv,
        })
        .ok_or_else(|| AuthError::NoForgejoToken {
            host: host.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    const ENTERPRISE: &str = "github.example.com";

    /// A `GhRunner` that records the argv it is handed and answers with a
    /// fixed canned output. Recording is the observation: it captures the
    /// slice at the boundary, so a dropped `--hostname` shows up here and
    /// nowhere else.
    struct RecordingGhRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        success: bool,
        stdout: String,
    }

    impl RecordingGhRunner {
        fn new(success: bool, stdout: &str) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                success,
                stdout: stdout.to_string(),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GhRunner for RecordingGhRunner {
        fn run_gh(
            &self,
            args: &[&str],
        ) -> impl std::future::Future<Output = Result<GhOutput, std::io::Error>> + Send {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|arg| (*arg).to_string()).collect());
            let output = GhOutput {
                success: self.success,
                stdout: self.stdout.clone(),
            };
            async move { Ok(output) }
        }
    }

    /// gh answers first, even when the host's environment variables are set.
    /// gh reads those variables itself, so consulting them here first would
    /// be indistinguishable in the common case — hence a fixture where the
    /// two disagree, which is the only one that can tell the orders apart.
    #[tokio::test]
    async fn gh_cli_wins_over_the_environment() {
        let gh = RecordingGhRunner::new(true, "gh-token\n");
        let found = resolve_token_with(GITHUB_COM, &gh, env(&[("GH_TOKEN", "env-token")]))
            .await
            .expect("a token should be found");
        assert_eq!(found.token, "gh-token");
        assert_eq!(found.source, TokenSource::GitHubCli);
    }

    /// The environment is the fallback for a gh that has nothing for the
    /// host, not a lower-priority alternative: a non-zero exit hands over,
    /// and whatever gh printed on the way out is not a token.
    #[tokio::test]
    async fn a_failed_gh_falls_back_to_the_environment() {
        let gh = RecordingGhRunner::new(false, "gh-token\n");
        let found = resolve_token_with(GITHUB_COM, &gh, env(&[("GH_TOKEN", "env-token")]))
            .await
            .expect("a token should be found");
        assert_eq!(found.token, "env-token");
        assert_eq!(found.source, TokenSource::GhTokenEnv);
    }

    /// `--hostname` reaches gh with the host being resolved. Without it gh
    /// answers for whichever host its own config names, so an Enterprise
    /// token can end up addressed to github.com.
    #[tokio::test]
    async fn gh_is_asked_about_the_host_being_resolved() {
        let gh = RecordingGhRunner::new(true, "gh-token\n");
        // The resolution outcome is not the assertion — the argv is.
        let _ = resolve_token_with(ENTERPRISE, &gh, env(&[])).await;
        assert_eq!(
            gh.calls(),
            vec![vec!["auth", "token", "--hostname", ENTERPRISE]]
        );
    }

    /// A `lookup` that resolves exactly the given name/value pairs.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn github_com_prefers_gh_token() {
        let found = token_from_env(GITHUB_COM, env(&[("GITHUB_TOKEN", "a"), ("GH_TOKEN", "b")]))
            .expect("a token should be found");
        assert_eq!(found.token, "b");
        assert_eq!(found.source, TokenSource::GhTokenEnv);
    }

    #[test]
    fn github_com_falls_back_to_github_token() {
        let found = token_from_env(GITHUB_COM, env(&[("GITHUB_TOKEN", "a")]))
            .expect("a token should be found");
        assert_eq!(found.token, "a");
        assert_eq!(found.source, TokenSource::GitHubTokenEnv);
    }

    #[test]
    fn github_com_ignores_the_enterprise_variables() {
        assert!(token_from_env(GITHUB_COM, env(&[("GH_ENTERPRISE_TOKEN", "e")])).is_none());
    }

    #[test]
    fn enterprise_prefers_gh_enterprise_token() {
        let found = token_from_env(
            ENTERPRISE,
            env(&[
                ("GH_ENTERPRISE_TOKEN", "e1"),
                ("GITHUB_ENTERPRISE_TOKEN", "e2"),
            ]),
        )
        .expect("a token should be found");
        assert_eq!(found.token, "e1");
        assert_eq!(found.source, TokenSource::GhEnterpriseTokenEnv);
    }

    #[test]
    fn enterprise_falls_back_to_github_enterprise_token() {
        let found = token_from_env(ENTERPRISE, env(&[("GITHUB_ENTERPRISE_TOKEN", "e2")]))
            .expect("a token should be found");
        assert_eq!(found.token, "e2");
        assert_eq!(found.source, TokenSource::GitHubEnterpriseTokenEnv);
    }

    #[test]
    fn enterprise_ignores_the_github_com_variables() {
        assert!(
            token_from_env(ENTERPRISE, env(&[("GITHUB_TOKEN", "a"), ("GH_TOKEN", "b")])).is_none()
        );
    }

    #[test]
    fn empty_values_are_skipped() {
        let found = token_from_env(GITHUB_COM, env(&[("GH_TOKEN", ""), ("GITHUB_TOKEN", "a")]))
            .expect("a token should be found");
        assert_eq!(found.token, "a");
        assert_eq!(found.source, TokenSource::GitHubTokenEnv);
    }

    #[test]
    fn auth_error_no_auth_found_is_actionable() {
        let err = AuthError::NoAuthFound {
            host: GITHUB_COM.to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("no GitHub authentication found"));
        assert!(msg.contains(GITHUB_COM));
        // Actionable advice is in the miette diagnostic help.
        let help = miette::Diagnostic::help(&err).expect("NoAuthFound should have diagnostic help");
        let help_text = help.to_string();
        assert!(help_text.contains("gh auth login --hostname github.com"));
        // The joined list, not the two names separately: it is what pins that
        // the help repeats `env_sources`' preference order.
        assert!(help_text.contains("GH_TOKEN/GITHUB_TOKEN"));
    }

    #[test]
    fn auth_error_names_the_enterprise_variables() {
        let err = AuthError::NoAuthFound {
            host: ENTERPRISE.to_string(),
        };
        let help = miette::Diagnostic::help(&err).expect("NoAuthFound should have diagnostic help");
        let help_text = help.to_string();
        assert!(help_text.contains("gh auth login --hostname github.example.com"));
        assert!(help_text.contains("GH_ENTERPRISE_TOKEN/GITHUB_ENTERPRISE_TOKEN"));
    }

    const FORGEJO_HOST: &str = "codeberg.org";

    #[test]
    fn forgejo_reads_forgejo_token() {
        let found = resolve_forgejo_token_with(FORGEJO_HOST, env(&[("FORGEJO_TOKEN", "f")]))
            .expect("a token should be found");
        assert_eq!(found.token, "f");
        assert_eq!(found.source, TokenSource::ForgejoTokenEnv);
    }

    #[test]
    fn forgejo_treats_an_empty_variable_as_unset() {
        let err = resolve_forgejo_token_with(FORGEJO_HOST, env(&[("FORGEJO_TOKEN", "")]))
            .expect_err("an empty token is no token");
        assert!(matches!(err, AuthError::NoForgejoToken { host } if host == FORGEJO_HOST));
    }

    /// The GitHub variables never reach a Forgejo host, whichever of the four
    /// is set.
    #[test]
    fn forgejo_ignores_the_github_variables() {
        let err = resolve_forgejo_token_with(
            FORGEJO_HOST,
            env(&[
                ("GH_TOKEN", "a"),
                ("GITHUB_TOKEN", "b"),
                ("GH_ENTERPRISE_TOKEN", "c"),
                ("GITHUB_ENTERPRISE_TOKEN", "d"),
            ]),
        )
        .expect_err("no Forgejo token is set");
        assert!(matches!(err, AuthError::NoForgejoToken { .. }));
    }

    #[test]
    fn auth_error_no_forgejo_token_is_actionable() {
        let err = AuthError::NoForgejoToken {
            host: FORGEJO_HOST.to_string(),
        };
        assert_eq!(err.to_string(), "no Forgejo token found for codeberg.org");
        let help = miette::Diagnostic::help(&err)
            .expect("NoForgejoToken should have diagnostic help")
            .to_string();
        assert!(help.contains("FORGEJO_TOKEN"), "{help}");
        assert!(help.contains("Settings → Applications"), "{help}");
        assert!(help.contains(FORGEJO_HOST), "{help}");
        // No `gh auth login`: gh knows nothing about Forgejo.
        assert!(!help.contains("gh "), "{help}");
    }
}
