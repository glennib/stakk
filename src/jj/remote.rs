//! GitHub remote URL parsing.

use crate::GITHUB_COM;

/// A parsed GitHub repository reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    /// Lowercased host the remote points at, e.g. `github.com` or
    /// `github.example.com`. Carries a port only for HTTP(S) remotes.
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    /// The REST API base URI for this repository's host.
    ///
    /// `None` for github.com, where octocrab's own default
    /// (`https://api.github.com`) is correct. GitHub Enterprise Server serves
    /// its API from `/api/v3` on the same host as the web UI.
    ///
    /// Always `https`, even for an `http://` remote: a GHES reachable only over
    /// plain HTTP is not supported.
    pub fn api_base_uri(&self) -> Option<String> {
        (self.host != GITHUB_COM).then(|| format!("https://{}/api/v3", self.host))
    }
}

impl std::fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// Whether a remote's host may be treated as a GitHub host.
///
/// github.com always qualifies. Any other host has to be named explicitly —
/// via `--github-host`, `STAKK_GITHUB_HOST`, `github_host` in `stakk.toml`, or
/// `GH_HOST` — so an unrelated forge is not mistaken for GitHub Enterprise
/// Server.
pub fn is_accepted_host(host: &str, extra_host: Option<&str>) -> bool {
    host == GITHUB_COM || extra_host.is_some_and(|extra| extra.eq_ignore_ascii_case(host))
}

/// Parse an owner/repo and its host from a remote URL, whatever the host.
///
/// Supports:
/// - HTTPS: `https://host/owner/repo.git` (also `http://`, embedded
///   credentials, and an explicit port)
/// - SSH (SCP-style): `git@host:owner/repo.git`
/// - SSH (canonical): `ssh://git@host/owner/repo.git` (also with a port)
/// - With or without `.git` suffix
///
/// The port is kept for HTTP(S) URLs, where it is also the port the API answers
/// on, and dropped for SSH URLs, where it is not.
///
/// Returns `None` for anything that is not a `<host>/<owner>/<repo>` URL. The
/// host is *not* checked here — see [`is_accepted_host`] and
/// [`parse_github_url`].
pub fn parse_remote_url(url: &str) -> Option<GitHubRepo> {
    // SSH canonical format: ssh://git@host:2222/owner/repo.git
    if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let host = strip_userinfo(authority);
        // An SSH port says nothing about where the API lives.
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        return build(host, path);
    }

    // HTTPS format: https://host/owner/repo.git
    for scheme in ["https://", "http://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            let (authority, path) = rest.split_once('/')?;
            // A port here is the one the API answers on too, so it stays.
            return build(strip_userinfo(authority), path);
        }
    }

    // SSH SCP-style format: git@host:owner/repo.git. The user part is required:
    // without it, `host:owner/repo` cannot be told apart from a scheme prefix.
    let (userinfo_host, path) = url.split_once(':')?;
    let (_, host) = userinfo_host.rsplit_once('@')?;
    build(host, path)
}

/// Parse a GitHub owner/repo from a remote URL, rejecting hosts that are not
/// github.com or `extra_host`.
pub fn parse_github_url(url: &str, extra_host: Option<&str>) -> Option<GitHubRepo> {
    parse_remote_url(url).filter(|parsed| is_accepted_host(&parsed.host, extra_host))
}

/// Drop a `user@` or `user:password@` prefix from a URL authority.
fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

fn build(host: &str, path: &str) -> Option<GitHubRepo> {
    if host.is_empty() {
        return None;
    }
    let (owner, repo) = parse_owner_repo(path)?;
    Some(GitHubRepo {
        host: host.to_ascii_lowercase(),
        owner,
        repo,
    })
}

fn parse_owner_repo(path: &str) -> Option<(String, String)> {
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);

    let mut parts = path.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;

    // Reject if there are additional path segments
    if parts.next().is_some() {
        return None;
    }

    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A github.com `GitHubRepo` for the common `glennib/stakk` case.
    fn stakk() -> GitHubRepo {
        GitHubRepo {
            host: GITHUB_COM.into(),
            owner: "glennib".into(),
            repo: "stakk".into(),
        }
    }

    const ENTERPRISE: &str = "github.example.com";

    #[test]
    fn https_with_git_suffix() {
        let result = parse_github_url("https://github.com/glennib/stakk.git", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn https_without_git_suffix() {
        let result = parse_github_url("https://github.com/glennib/stakk", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_with_git_suffix() {
        let result = parse_github_url("git@github.com:glennib/stakk.git", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_without_git_suffix() {
        let result = parse_github_url("git@github.com:glennib/stakk", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn https_with_trailing_slash() {
        let result = parse_github_url("https://github.com/owner/repo/", None);
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: GITHUB_COM.into(),
                owner: "owner".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn non_github_https() {
        let result = parse_github_url("https://gitlab.com/owner/repo.git", None);
        assert_eq!(result, None);
    }

    #[test]
    fn non_github_ssh() {
        let result = parse_github_url("git@gitlab.com:owner/repo.git", None);
        assert_eq!(result, None);
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_github_url("", None), None);
    }

    #[test]
    fn missing_repo() {
        assert_eq!(parse_github_url("https://github.com/owner", None), None);
    }

    #[test]
    fn extra_path_segments() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/extra", None),
            None
        );
    }

    #[test]
    fn ssh_canonical_with_git_suffix() {
        let result = parse_github_url("ssh://git@github.com/glennib/stakk.git", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_canonical_without_git_suffix() {
        let result = parse_github_url("ssh://git@github.com/glennib/stakk", None);
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn non_github_ssh_canonical() {
        assert_eq!(
            parse_github_url("ssh://git@gitlab.com/owner/repo.git", None),
            None
        );
    }

    #[test]
    fn enterprise_https() {
        let result = parse_github_url("https://github.example.com/org/repo.git", Some(ENTERPRISE));
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: ENTERPRISE.into(),
                owner: "org".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn enterprise_ssh_scp_style() {
        let result = parse_github_url("git@github.example.com:org/repo.git", Some(ENTERPRISE));
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: ENTERPRISE.into(),
                owner: "org".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn enterprise_ssh_canonical_without_git_suffix() {
        let result = parse_github_url("ssh://git@github.example.com/org/repo", Some(ENTERPRISE));
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: ENTERPRISE.into(),
                owner: "org".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn enterprise_rejected_without_configuration() {
        assert_eq!(
            parse_github_url("git@github.example.com:org/repo.git", None),
            None
        );
    }

    #[test]
    fn github_com_still_accepted_when_enterprise_host_configured() {
        let result = parse_github_url("git@github.com:glennib/stakk.git", Some(ENTERPRISE));
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn configured_host_match_is_case_insensitive() {
        let result = parse_github_url("git@GitHub.Example.COM:org/repo.git", Some(ENTERPRISE));
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: ENTERPRISE.into(),
                owner: "org".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn ssh_port_is_dropped() {
        let result = parse_remote_url("ssh://git@github.example.com:2222/org/repo.git");
        assert_eq!(result.map(|r| r.host), Some(ENTERPRISE.to_string()));
    }

    #[test]
    fn https_port_is_kept() {
        let result = parse_remote_url("https://github.example.com:8443/org/repo.git");
        assert_eq!(
            result.map(|r| r.host),
            Some("github.example.com:8443".to_string())
        );
    }

    #[test]
    fn https_credentials_are_stripped() {
        let result = parse_remote_url("https://user:token@github.example.com/org/repo.git");
        assert_eq!(result.map(|r| r.host), Some(ENTERPRISE.to_string()));
    }

    #[test]
    fn parse_remote_url_accepts_any_host() {
        let result = parse_remote_url("git@gitlab.com:owner/repo.git");
        assert_eq!(
            result,
            Some(GitHubRepo {
                host: "gitlab.com".into(),
                owner: "owner".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn parse_remote_url_rejects_non_url() {
        assert_eq!(parse_remote_url("not a url"), None);
        assert_eq!(parse_remote_url("/some/local/path"), None);
    }

    #[test]
    fn api_base_uri_is_none_for_github_com() {
        assert_eq!(stakk().api_base_uri(), None);
    }

    #[test]
    fn api_base_uri_is_v3_for_enterprise() {
        let repo = GitHubRepo {
            host: ENTERPRISE.into(),
            owner: "org".into(),
            repo: "repo".into(),
        };
        assert_eq!(
            repo.api_base_uri().as_deref(),
            Some("https://github.example.com/api/v3")
        );
    }

    #[test]
    fn api_base_uri_keeps_an_https_port() {
        let repo = GitHubRepo {
            host: "github.example.com:8443".into(),
            owner: "org".into(),
            repo: "repo".into(),
        };
        assert_eq!(
            repo.api_base_uri().as_deref(),
            Some("https://github.example.com:8443/api/v3")
        );
    }
}
