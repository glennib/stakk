//! jj CLI interface.
//!
//! All VCS operations go through this module by shelling out to `jj`. No direct
//! git calls, no `git2`, no `gix`. Always pass `--config 'ui.paginate=never'`
//! to avoid pager issues.

pub mod remote;
pub mod runner;
pub mod types;

use miette::Diagnostic;
use thiserror::Error;

use crate::jj::runner::JjRunner;
use crate::jj::types::Bookmark;
use crate::jj::types::BookmarkEntryRaw;
use crate::jj::types::GitRemote;
use crate::jj::types::LogEntry;
use crate::jj::types::LogEntryRaw;

/// Errors from interacting with `jj`.
#[derive(Debug, Error, Diagnostic)]
pub enum JjError {
    /// The `jj` command exited with a non-zero status.
    #[error("jj command failed: {stderr}")]
    CommandFailed { stderr: String },

    /// Failed to parse `jj` output.
    #[error("failed to parse jj output ({context}): {source}")]
    ParseError {
        context: String,
        source: serde_json::Error,
    },

    /// `jj` binary not found.
    #[error("could not run jj: {0}")]
    #[diagnostic(help("Make sure jj is installed and available on your PATH"))]
    NotFound(std::io::Error),

    /// Could not determine the default branch.
    #[error(
        "could not determine default branch; trunk() remote bookmark candidates: {candidates:?}"
    )]
    NoDefaultBranch { candidates: Vec<String> },
}

// Template for `jj bookmark list`: produces one JSON object per line.
const BOOKMARK_TEMPLATE: &str = r#""{\"name\":" ++ json(self.name()) ++ ",\"synced\":" ++ json(self.synced()) ++ ",\"target\":" ++ json(self.normal_target()) ++ "}\n""#;

// Template for `jj log`: produces one JSON object per line with commit +
// bookmarks.
const LOG_TEMPLATE: &str = r#""{\"commit\":" ++ json(self) ++ ",\"local_bookmarks\":" ++ json(local_bookmarks) ++ ",\"remote_bookmarks\":" ++ json(remote_bookmarks) ++ "}\n""#;

/// Main interface for interacting with `jj`.
pub struct Jj<R: JjRunner> {
    runner: R,
}

impl<R: JjRunner> Jj<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// List bookmarks belonging to the current user, excluding trunk.
    pub async fn get_my_bookmarks(&self) -> Result<Vec<Bookmark>, JjError> {
        let output = self
            .runner
            .run_jj(&[
                "bookmark",
                "list",
                "-r",
                "mine() ~ trunk()",
                "-T",
                BOOKMARK_TEMPLATE,
            ])
            .await?;

        parse_bookmarks(&output)
    }

    /// Get log entries for a revision range, paginated.
    pub async fn get_branch_changes_paginated(
        &self,
        trunk: &str,
        to: &str,
        last_seen: Option<&str>,
    ) -> Result<Vec<LogEntry>, JjError> {
        let revset = match last_seen {
            Some(last) => format!("({trunk}..{to}) ~ {last}::"),
            None => format!("{trunk}..{to}"),
        };

        let output = self
            .runner
            .run_jj(&[
                "log",
                "-r",
                &revset,
                "--no-graph",
                "--limit",
                "100",
                "-T",
                LOG_TEMPLATE,
            ])
            .await?;

        parse_log_entries(&output)
    }

    /// List git remotes.
    pub async fn get_git_remote_list(&self) -> Result<Vec<GitRemote>, JjError> {
        let output = self.runner.run_jj(&["git", "remote", "list"]).await?;
        Ok(parse_git_remote_list(&output))
    }

    /// Detect the default branch name from `trunk()`.
    pub async fn get_default_branch(&self) -> Result<String, JjError> {
        let output = self
            .runner
            .run_jj(&[
                "log",
                "-r",
                "trunk()",
                "--no-graph",
                "--limit",
                "1",
                "-T",
                LOG_TEMPLATE,
            ])
            .await?;

        let entries = parse_log_entries(&output)?;
        let entry = entries
            .first()
            .ok_or_else(|| JjError::NoDefaultBranch { candidates: vec![] })?;

        // Filter out the internal "git" remote — we want the real remote name
        // like "origin".
        let candidates: Vec<&str> = entry
            .remote_bookmark_names
            .iter()
            .filter(|name| {
                // Remote bookmark names from jj include entries like
                // "main@origin" and "main@git". Filter for non-"git" remotes.
                !name.ends_with("@git")
            })
            .map(|s| s.as_str())
            .collect();

        match candidates.first() {
            Some(name) => {
                // Strip the "@remote" suffix to get just the branch name.
                let branch = name.split('@').next().unwrap_or(name);
                Ok(branch.to_string())
            }
            None => Err(JjError::NoDefaultBranch {
                candidates: entry.remote_bookmark_names.clone(),
            }),
        }
    }

    /// Push a bookmark to a remote.
    pub async fn push_bookmark(&self, bookmark: &str, remote: &str) -> Result<(), JjError> {
        self.runner
            .run_jj(&[
                "git",
                "push",
                "--remote",
                remote,
                "--bookmark",
                bookmark,
                "--allow-new",
            ])
            .await?;
        Ok(())
    }

    /// Fetch from all remotes.
    #[expect(
        dead_code,
        reason = "available for pre-submission fetch in a future milestone"
    )]
    pub async fn git_fetch(&self) -> Result<(), JjError> {
        self.runner
            .run_jj(&["git", "fetch", "--all-remotes"])
            .await?;
        Ok(())
    }
}

fn parse_bookmarks(output: &str) -> Result<Vec<Bookmark>, JjError> {
    let mut seen = std::collections::HashSet::new();
    let mut bookmarks = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let raw: BookmarkEntryRaw =
            serde_json::from_str(line).map_err(|e| JjError::ParseError {
                context: "bookmark list".to_string(),
                source: e,
            })?;
        // When a bookmark is unsynced, jj emits separate entries for the local
        // and remote tracking targets. Keep only the first (local) entry.
        if !seen.insert(raw.name.clone()) {
            continue;
        }
        // Skip conflicted bookmarks (no normal target).
        if let Some(target) = raw.target {
            bookmarks.push(Bookmark {
                name: raw.name,
                commit_id: target.commit_id,
                change_id: target.change_id,
                synced: raw.synced,
            });
        }
    }
    Ok(bookmarks)
}

fn parse_log_entries(output: &str) -> Result<Vec<LogEntry>, JjError> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let raw: LogEntryRaw = serde_json::from_str(line).map_err(|e| JjError::ParseError {
            context: "log entry".to_string(),
            source: e,
        })?;
        entries.push(LogEntry {
            commit_id: raw.commit.commit_id,
            change_id: raw.commit.change_id,
            description: raw.commit.description,
            parents: raw.commit.parents,
            author: raw.commit.author,
            local_bookmark_names: raw.local_bookmarks.iter().map(|b| b.name.clone()).collect(),
            remote_bookmark_names: raw
                .remote_bookmarks
                .iter()
                .map(|b| match &b.remote {
                    Some(remote) => format!("{}@{}", b.name, remote),
                    None => b.name.clone(),
                })
                .collect(),
        });
    }
    Ok(entries)
}

fn parse_git_remote_list(output: &str) -> Vec<GitRemote> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, char::is_whitespace);
            let name = parts.next()?.trim();
            let url = parts.next()?.trim();
            if name.is_empty() || url.is_empty() {
                return None;
            }
            Some(GitRemote {
                name: name.to_string(),
                url: url.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_bookmarks tests --

    #[test]
    fn parse_bookmarks_single() {
        let input = r#"{"name":"feature","synced":false,"target":{"commit_id":"abc123","parents":["def456"],"change_id":"xyz789","description":"my feature\n","author":{"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"},"committer":{"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"}}}"#;
        let bookmarks = parse_bookmarks(input).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "feature");
        assert_eq!(bookmarks[0].commit_id, "abc123");
        assert_eq!(bookmarks[0].change_id, "xyz789");
        assert!(!bookmarks[0].synced);
    }

    #[test]
    fn parse_bookmarks_multiple() {
        let input = concat!(
            r#"{"name":"a","synced":true,"target":{"commit_id":"111","parents":[],"change_id":"aaa","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#,
            "\n",
            r#"{"name":"b","synced":false,"target":{"commit_id":"222","parents":[],"change_id":"bbb","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#,
        );
        let bookmarks = parse_bookmarks(input).unwrap();
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].name, "a");
        assert_eq!(bookmarks[1].name, "b");
    }

    #[test]
    fn parse_bookmarks_conflicted_skipped() {
        let input = r#"{"name":"conflict","synced":false,"target":null}"#;
        let bookmarks = parse_bookmarks(input).unwrap();
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn parse_bookmarks_empty_input() {
        let bookmarks = parse_bookmarks("").unwrap();
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn parse_bookmarks_deduplicates_unsynced() {
        // When a bookmark is unsynced, jj emits two entries: local and remote
        // tracking target. We should keep only the first (local) entry.
        let local = r#"{"name":"feat","synced":false,"target":{"commit_id":"new","parents":[],"change_id":"x1","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#;
        let remote = r#"{"name":"feat","synced":false,"target":{"commit_id":"old","parents":[],"change_id":"x2","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#;
        let input = format!("{local}\n{remote}");
        let bookmarks = parse_bookmarks(&input).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "feat");
        assert_eq!(bookmarks[0].commit_id, "new");
    }

    // -- parse_log_entries tests --

    #[test]
    fn parse_log_entries_single() {
        let input = r#"{"commit":{"commit_id":"abc","parents":["def"],"change_id":"xyz","description":"desc\n","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[{"name":"feat","target":["abc"]}],"remote_bookmarks":[{"name":"feat","remote":"origin","target":["abc"],"tracking_target":["abc"]}]}"#;
        let entries = parse_log_entries(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit_id, "abc");
        assert_eq!(entries[0].local_bookmark_names, vec!["feat"]);
        assert_eq!(entries[0].remote_bookmark_names, vec!["feat@origin"]);
    }

    #[test]
    fn parse_log_entries_no_bookmarks() {
        let input = r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"remote_bookmarks":[]}"#;
        let entries = parse_log_entries(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].local_bookmark_names.is_empty());
        assert!(entries[0].remote_bookmark_names.is_empty());
    }

    #[test]
    fn parse_log_entries_multiple() {
        let line1 = r#"{"commit":{"commit_id":"aaa","parents":[],"change_id":"x1","description":"first","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"remote_bookmarks":[]}"#;
        let line2 = r#"{"commit":{"commit_id":"bbb","parents":["aaa"],"change_id":"x2","description":"second","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"remote_bookmarks":[]}"#;
        let input = format!("{line1}\n{line2}");
        let entries = parse_log_entries(&input).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].commit_id, "aaa");
        assert_eq!(entries[1].commit_id, "bbb");
    }

    // -- parse_git_remote_list tests --

    #[test]
    fn parse_git_remote_list_single() {
        let input = "origin git@github.com:glennib/stakk.git\n";
        let remotes = parse_git_remote_list(input);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].url, "git@github.com:glennib/stakk.git");
    }

    #[test]
    fn parse_git_remote_list_multiple() {
        let input =
            "origin git@github.com:owner/repo.git\nupstream https://github.com/other/repo.git\n";
        let remotes = parse_git_remote_list(input);
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[1].name, "upstream");
        assert_eq!(remotes[1].url, "https://github.com/other/repo.git");
    }

    #[test]
    fn parse_git_remote_list_empty() {
        let remotes = parse_git_remote_list("");
        assert!(remotes.is_empty());
    }

    // -- Integration tests with mock runner --

    struct MockJjRunner<F: Fn(&[&str]) -> Result<String, JjError> + Send + Sync> {
        handler: F,
    }

    impl<F> JjRunner for MockJjRunner<F>
    where
        F: Fn(&[&str]) -> Result<String, JjError> + Send + Sync,
    {
        async fn run_jj(&self, args: &[&str]) -> Result<String, JjError> {
            (self.handler)(args)
        }
    }

    #[tokio::test]
    async fn get_my_bookmarks_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "bookmark");
                assert_eq!(args[1], "list");
                Ok(r#"{"name":"my-feature","synced":false,"target":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"feat","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let bookmarks = jj.get_my_bookmarks().await.unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "my-feature");
    }

    #[tokio::test]
    async fn get_git_remote_list_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "git");
                assert_eq!(args[1], "remote");
                Ok("origin git@github.com:glennib/stakk.git\n".to_string())
            },
        };
        let jj = Jj::new(runner);
        let remotes = jj.get_git_remote_list().await.unwrap();
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "origin");
    }

    #[tokio::test]
    async fn get_default_branch_integration() {
        let runner = MockJjRunner {
            handler: |_args: &[&str]| {
                Ok(r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[{"name":"main","target":["abc"]}],"remote_bookmarks":[{"name":"main","remote":"git","target":["abc"],"tracking_target":["abc"]},{"name":"main","remote":"origin","target":["abc"],"tracking_target":["abc"]}]}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let branch = jj.get_default_branch().await.unwrap();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn get_default_branch_no_remote_bookmarks() {
        let runner = MockJjRunner {
            handler: |_args: &[&str]| {
                Ok(r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"remote_bookmarks":[]}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let result = jj.get_default_branch().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_branch_changes_paginated_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "log");
                // Check the revset is constructed correctly
                let revset = args[2];
                assert!(revset.contains(".."));
                Ok(r#"{"commit":{"commit_id":"c1","parents":["c0"],"change_id":"ch1","description":"change 1","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"remote_bookmarks":[]}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let entries = jj
            .get_branch_changes_paginated("main", "feature", None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit_id, "c1");
    }

    #[tokio::test]
    async fn get_branch_changes_with_last_seen() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                let revset = args[2];
                assert!(
                    revset.contains("~ prev::"),
                    "expected last_seen exclusion in revset: {revset}"
                );
                Ok(String::new())
            },
        };
        let jj = Jj::new(runner);
        let entries = jj
            .get_branch_changes_paginated("main", "feature", Some("prev"))
            .await
            .unwrap();
        assert!(entries.is_empty());
    }
}
