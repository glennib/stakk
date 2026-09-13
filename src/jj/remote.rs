//! Remote URL parsing.

use crate::GITHUB_COM;

/// URL scheme a forge API is reached over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl std::fmt::Display for Scheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Http => "http",
            Self::Https => "https",
        })
    }
}

/// A parsed `<host>/<owner>/<repo>` remote, on any host.
///
/// Nothing here is forge-specific: [`parse_remote_url`] fills it for every
/// host, and which forge that host runs is `crate::forge::detect`'s question,
/// asked afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepo {
    /// Lowercased host the remote points at, e.g. `github.com` or
    /// `github.example.com`. Carries a port only for HTTP(S) remotes.
    pub host: String,
    pub owner: String,
    pub repo: String,
    /// The remote URL's scheme. HTTP(S) remotes record theirs; SSH remotes
    /// (both forms) record `https`, since the API is not reachable over SSH.
    /// Read by [`RemoteRepo::forgejo_api_base_uri`] only —
    /// [`RemoteRepo::github_api_base_uri`] is `https` regardless.
    pub scheme: Scheme,
}

impl RemoteRepo {
    /// The GitHub REST API base URI for this repository's host.
    ///
    /// `None` for github.com, where octocrab's own default
    /// (`https://api.github.com`) is correct. GitHub Enterprise Server serves
    /// its API from `/api/v3` on the same host as the web UI.
    ///
    /// Always `https`, even for an `http://` remote: a GHES reachable only over
    /// plain HTTP is not supported.
    pub fn github_api_base_uri(&self) -> Option<String> {
        (self.host != GITHUB_COM).then(|| format!("https://{}/api/v3", self.host))
    }

    /// The Forgejo REST API base URI for this repository's host.
    ///
    /// Forgejo serves its API from `/api/v1` on the same host as the web UI,
    /// over the remote's own scheme — a plain-`http` instance on a `host:port`
    /// is a supported (if local) setup. Always a value: unlike octocrab there
    /// is no client default to fall back on, not even for codeberg.org.
    pub fn forgejo_api_base_uri(&self) -> String {
        format!("{}://{}/api/v1", self.scheme, self.host)
    }
}

impl std::fmt::Display for RemoteRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
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
/// host is not judged here: every host parses, and the result's `host` is
/// lowercased so callers can compare it.
pub fn parse_remote_url(url: &str) -> Option<RemoteRepo> {
    // SSH canonical format: ssh://git@host:2222/owner/repo.git
    if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let host = strip_userinfo(authority);
        // An SSH port says nothing about where the API lives.
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        return build(host, path, Scheme::Https);
    }

    // HTTPS format: https://host/owner/repo.git
    for (prefix, scheme) in [("https://", Scheme::Https), ("http://", Scheme::Http)] {
        if let Some(rest) = url.strip_prefix(prefix) {
            let (authority, path) = rest.split_once('/')?;
            // A port here is the one the API answers on too, so it stays.
            return build(strip_userinfo(authority), path, scheme);
        }
    }

    // SSH SCP-style format: git@host:owner/repo.git. The user part is required:
    // without it, `host:owner/repo` cannot be told apart from a scheme prefix.
    let (userinfo_host, path) = url.split_once(':')?;
    let (_, host) = userinfo_host.rsplit_once('@')?;
    build(host, path, Scheme::Https)
}

/// Drop a `user@` or `user:password@` prefix from a URL authority.
fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

fn build(host: &str, path: &str, scheme: Scheme) -> Option<RemoteRepo> {
    if host.is_empty() {
        return None;
    }
    let (owner, repo) = parse_owner_repo(path)?;
    Some(RemoteRepo {
        host: host.to_ascii_lowercase(),
        owner,
        repo,
        scheme,
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

    /// A github.com `RemoteRepo` for the common `glennib/stakk` case.
    fn stakk() -> RemoteRepo {
        RemoteRepo {
            host: GITHUB_COM.into(),
            owner: "glennib".into(),
            repo: "stakk".into(),
            scheme: Scheme::Https,
        }
    }

    const ENTERPRISE: &str = "github.example.com";

    #[test]
    fn https_with_git_suffix() {
        let result = parse_remote_url("https://github.com/glennib/stakk.git");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn https_without_git_suffix() {
        let result = parse_remote_url("https://github.com/glennib/stakk");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_with_git_suffix() {
        let result = parse_remote_url("git@github.com:glennib/stakk.git");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_without_git_suffix() {
        let result = parse_remote_url("git@github.com:glennib/stakk");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn https_with_trailing_slash() {
        let result = parse_remote_url("https://github.com/owner/repo/");
        assert_eq!(
            result,
            Some(RemoteRepo {
                host: GITHUB_COM.into(),
                owner: "owner".into(),
                repo: "repo".into(),
                scheme: Scheme::Https,
            })
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_remote_url(""), None);
    }

    #[test]
    fn missing_repo() {
        assert_eq!(parse_remote_url("https://github.com/owner"), None);
    }

    #[test]
    fn extra_path_segments() {
        assert_eq!(
            parse_remote_url("https://github.com/owner/repo/extra"),
            None
        );
    }

    #[test]
    fn ssh_canonical_with_git_suffix() {
        let result = parse_remote_url("ssh://git@github.com/glennib/stakk.git");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn ssh_canonical_without_git_suffix() {
        let result = parse_remote_url("ssh://git@github.com/glennib/stakk");
        assert_eq!(result, Some(stakk()));
    }

    #[test]
    fn host_is_lowercased() {
        let result = parse_remote_url("git@GitHub.Example.COM:org/repo.git");
        assert_eq!(result.map(|r| r.host), Some(ENTERPRISE.to_string()));
    }

    #[test]
    fn http_remote_keeps_scheme_and_port() {
        assert_eq!(
            parse_remote_url("http://localhost:3000/stakk/probe.git"),
            Some(RemoteRepo {
                host: "localhost:3000".into(),
                owner: "stakk".into(),
                repo: "probe".into(),
                scheme: Scheme::Http,
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
            Some(RemoteRepo {
                host: "gitlab.com".into(),
                owner: "owner".into(),
                repo: "repo".into(),
                scheme: Scheme::Https,
            })
        );
    }

    #[test]
    fn parse_remote_url_rejects_non_url() {
        assert_eq!(parse_remote_url("not a url"), None);
        assert_eq!(parse_remote_url("/some/local/path"), None);
    }

    #[test]
    fn github_api_base_uri_is_none_for_github_com() {
        assert_eq!(stakk().github_api_base_uri(), None);
    }

    #[test]
    fn github_api_base_uri_is_v3_for_enterprise() {
        let repo = RemoteRepo {
            host: ENTERPRISE.into(),
            owner: "org".into(),
            repo: "repo".into(),
            scheme: Scheme::Https,
        };
        assert_eq!(
            repo.github_api_base_uri().as_deref(),
            Some("https://github.example.com/api/v3")
        );
    }

    #[test]
    fn github_api_base_uri_keeps_an_https_port() {
        let repo = RemoteRepo {
            host: "github.example.com:8443".into(),
            owner: "org".into(),
            repo: "repo".into(),
            scheme: Scheme::Https,
        };
        assert_eq!(
            repo.github_api_base_uri().as_deref(),
            Some("https://github.example.com:8443/api/v3")
        );
    }

    // ---- Scheme and the Forgejo API base ----

    #[test]
    fn scheme_is_recorded_per_url_form() {
        let scheme = |url: &str| parse_remote_url(url).map(|r| r.scheme);
        assert_eq!(scheme("http://localhost:3000/o/r.git"), Some(Scheme::Http));
        assert_eq!(scheme("https://codeberg.org/o/r.git"), Some(Scheme::Https));
        // SSH says nothing about the API's scheme; https is the assumption.
        assert_eq!(scheme("git@codeberg.org:o/r.git"), Some(Scheme::Https));
        assert_eq!(
            scheme("ssh://git@codeberg.org:2222/o/r.git"),
            Some(Scheme::Https)
        );
    }

    #[test]
    fn scheme_displays_as_the_url_prefix() {
        assert_eq!(Scheme::Http.to_string(), "http");
        assert_eq!(Scheme::Https.to_string(), "https");
    }

    #[test]
    fn forgejo_api_base_uri_keeps_http_and_a_port() {
        let repo = parse_remote_url("http://localhost:3000/stakk/probe.git").unwrap();
        assert_eq!(repo.forgejo_api_base_uri(), "http://localhost:3000/api/v1");
    }

    #[test]
    fn forgejo_api_base_uri_turns_ssh_into_https() {
        let repo = parse_remote_url("git@codeberg.org:owner/repo.git").unwrap();
        assert_eq!(repo.forgejo_api_base_uri(), "https://codeberg.org/api/v1");
        let repo = parse_remote_url("ssh://git@git.example.com:2222/owner/repo.git").unwrap();
        assert_eq!(
            repo.forgejo_api_base_uri(),
            "https://git.example.com/api/v1"
        );
    }

    #[test]
    fn forgejo_api_base_uri_has_no_special_case_for_codeberg() {
        let repo = parse_remote_url("https://codeberg.org/owner/repo.git").unwrap();
        assert_eq!(repo.forgejo_api_base_uri(), "https://codeberg.org/api/v1");
    }

    /// The recorded scheme is a Forgejo concern: a GHES over plain HTTP stays
    /// unsupported, as `github_api_base_uri` documents.
    #[test]
    fn github_api_base_uri_stays_https_for_an_http_remote() {
        let repo = parse_remote_url("http://github.example.com/org/repo.git").unwrap();
        assert_eq!(
            repo.github_api_base_uri().as_deref(),
            Some("https://github.example.com/api/v3")
        );
    }
}
