//! GitHub remote URL parsing.

/// A parsed GitHub repository reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
}

impl std::fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// Parse a GitHub owner/repo from a remote URL.
///
/// Supports:
/// - HTTPS: `https://github.com/owner/repo.git`
/// - SSH: `git@github.com:owner/repo.git`
/// - With or without `.git` suffix
///
/// Returns `None` for non-GitHub URLs.
pub fn parse_github_url(url: &str) -> Option<GitHubRepo> {
    // SSH format: git@github.com:owner/repo.git
    if let Some(path) = url.strip_prefix("git@github.com:") {
        return parse_owner_repo(path);
    }

    // HTTPS format: https://github.com/owner/repo.git
    let url_without_scheme = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;

    parse_owner_repo(url_without_scheme)
}

fn parse_owner_repo(path: &str) -> Option<GitHubRepo> {
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);

    let mut parts = path.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;

    // Reject if there are additional path segments
    if parts.next().is_some() {
        return None;
    }

    Some(GitHubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_with_git_suffix() {
        let result = parse_github_url("https://github.com/glennib/stakk.git");
        assert_eq!(
            result,
            Some(GitHubRepo {
                owner: "glennib".into(),
                repo: "stakk".into(),
            })
        );
    }

    #[test]
    fn https_without_git_suffix() {
        let result = parse_github_url("https://github.com/glennib/stakk");
        assert_eq!(
            result,
            Some(GitHubRepo {
                owner: "glennib".into(),
                repo: "stakk".into(),
            })
        );
    }

    #[test]
    fn ssh_with_git_suffix() {
        let result = parse_github_url("git@github.com:glennib/stakk.git");
        assert_eq!(
            result,
            Some(GitHubRepo {
                owner: "glennib".into(),
                repo: "stakk".into(),
            })
        );
    }

    #[test]
    fn ssh_without_git_suffix() {
        let result = parse_github_url("git@github.com:glennib/stakk");
        assert_eq!(
            result,
            Some(GitHubRepo {
                owner: "glennib".into(),
                repo: "stakk".into(),
            })
        );
    }

    #[test]
    fn https_with_trailing_slash() {
        let result = parse_github_url("https://github.com/owner/repo/");
        assert_eq!(
            result,
            Some(GitHubRepo {
                owner: "owner".into(),
                repo: "repo".into(),
            })
        );
    }

    #[test]
    fn non_github_https() {
        let result = parse_github_url("https://gitlab.com/owner/repo.git");
        assert_eq!(result, None);
    }

    #[test]
    fn non_github_ssh() {
        let result = parse_github_url("git@gitlab.com:owner/repo.git");
        assert_eq!(result, None);
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_github_url(""), None);
    }

    #[test]
    fn missing_repo() {
        assert_eq!(parse_github_url("https://github.com/owner"), None);
    }

    #[test]
    fn extra_path_segments() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/extra"),
            None
        );
    }
}
