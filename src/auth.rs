//! GitHub authentication token resolution.
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
            reason = "the resolution tests read it to pin which token environment variable wins \
                      for a given host"
        )
    )]
    pub source: TokenSource,
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
    if let Some(token) = try_gh_cli(host).await? {
        return Ok(AuthToken {
            token,
            source: TokenSource::GitHubCli,
        });
    }

    token_from_env(host, |name| std::env::var(name).ok()).ok_or_else(|| AuthError::NoAuthFound {
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
async fn try_gh_cli(host: &str) -> Result<Option<String>, AuthError> {
    let result = tokio::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    const ENTERPRISE: &str = "github.example.com";

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
}
