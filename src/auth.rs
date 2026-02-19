//! GitHub authentication token resolution.
//!
//! Resolves a token in priority order:
//! 1. `gh auth token` (GitHub CLI)
//! 2. `GITHUB_TOKEN` environment variable
//! 3. `GH_TOKEN` environment variable

use miette::Diagnostic;
use thiserror::Error;

/// How the token was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// From `gh auth token`.
    GitHubCli,
    /// From `GITHUB_TOKEN` environment variable.
    GitHubTokenEnv,
    /// From `GH_TOKEN` environment variable.
    GhTokenEnv,
}

impl std::fmt::Display for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHubCli => write!(f, "GitHub CLI (gh auth token)"),
            Self::GitHubTokenEnv => write!(f, "GITHUB_TOKEN environment variable"),
            Self::GhTokenEnv => write!(f, "GH_TOKEN environment variable"),
        }
    }
}

/// A resolved authentication token with its source.
#[derive(Debug, Clone)]
pub struct AuthToken {
    pub token: String,
    pub source: TokenSource,
}

/// Errors from authentication resolution.
#[derive(Debug, Error, Diagnostic)]
pub enum AuthError {
    #[error("no GitHub authentication found")]
    #[diagnostic(help("Run `gh auth login` or set GITHUB_TOKEN/GH_TOKEN"))]
    NoAuthFound,

    #[error("failed to run `gh auth token`: {0}")]
    GhCliError(std::io::Error),
}

/// Resolve a GitHub authentication token.
///
/// Tries sources in order: gh CLI, GITHUB_TOKEN env, GH_TOKEN env.
/// Returns the first token found, or `AuthError::NoAuthFound`.
///
/// This does NOT validate the token against the GitHub API.
/// Use `Forge::get_authenticated_user()` to validate.
pub async fn resolve_token() -> Result<AuthToken, AuthError> {
    // 1. Try `gh auth token`
    if let Some(token) = try_gh_cli().await? {
        return Ok(AuthToken {
            token,
            source: TokenSource::GitHubCli,
        });
    }

    // 2. Try GITHUB_TOKEN
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.is_empty()
    {
        return Ok(AuthToken {
            token,
            source: TokenSource::GitHubTokenEnv,
        });
    }

    // 3. Try GH_TOKEN
    if let Ok(token) = std::env::var("GH_TOKEN")
        && !token.is_empty()
    {
        return Ok(AuthToken {
            token,
            source: TokenSource::GhTokenEnv,
        });
    }

    Err(AuthError::NoAuthFound)
}

/// Try to get a token from the GitHub CLI.
///
/// Returns `Ok(None)` if gh is not installed or not authenticated.
/// Returns `Err` only for unexpected I/O failures.
async fn try_gh_cli() -> Result<Option<String>, AuthError> {
    let result = tokio::process::Command::new("gh")
        .args(["auth", "token"])
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

    #[test]
    fn token_source_display_github_cli() {
        assert_eq!(
            TokenSource::GitHubCli.to_string(),
            "GitHub CLI (gh auth token)"
        );
    }

    #[test]
    fn token_source_display_github_token_env() {
        assert_eq!(
            TokenSource::GitHubTokenEnv.to_string(),
            "GITHUB_TOKEN environment variable"
        );
    }

    #[test]
    fn token_source_display_gh_token_env() {
        assert_eq!(
            TokenSource::GhTokenEnv.to_string(),
            "GH_TOKEN environment variable"
        );
    }

    #[test]
    fn auth_error_no_auth_found_is_actionable() {
        let err = AuthError::NoAuthFound;
        let msg = err.to_string();
        assert!(msg.contains("no GitHub authentication found"));
        // Actionable advice is in the miette diagnostic help.
        let help = miette::Diagnostic::help(&err).expect("NoAuthFound should have diagnostic help");
        let help_text = help.to_string();
        assert!(help_text.contains("gh auth login"));
        assert!(help_text.contains("GITHUB_TOKEN"));
        assert!(help_text.contains("GH_TOKEN"));
    }
}
