//! jj CLI interface.
//!
//! All VCS operations go through this module by shelling out to `jj`. No direct
//! git calls, no `git2`, no `gix`. Always pass `--config 'ui.paginate=never'`
//! to avoid pager issues.

pub mod remote;
pub mod runner;
pub mod types;
pub mod version;

use std::collections::HashSet;

use miette::Diagnostic;
use thiserror::Error;

use crate::jj::runner::JjRunner;
use crate::jj::types::Bookmark;
use crate::jj::types::BookmarkEntryRaw;
use crate::jj::types::CommitRefData;
use crate::jj::types::GitRemote;
use crate::jj::types::LogEntry;
use crate::jj::types::LogEntryRaw;
use crate::jj::version::JjVersion;

/// Errors from interacting with `jj`.
#[derive(Debug, Error, Diagnostic)]
pub enum JjError {
    /// The `jj` command exited with a non-zero status.
    #[error("jj command failed: {command}\n{stderr}")]
    #[diagnostic(code(stakk::jj::command_failed))]
    CommandFailed { command: String, stderr: String },

    /// Failed to parse `jj` output.
    #[error("failed to parse jj output ({context}): {source}")]
    #[diagnostic(
        code(stakk::jj::parse_error),
        help(
            "jj's output may have changed shape — this can indicate a jj/stakk version \
             incompatibility; check `jj --version`"
        )
    )]
    ParseError {
        context: String,
        source: serde_json::Error,
    },

    /// `jj` binary not found.
    #[error("could not run jj: {0}")]
    #[diagnostic(
        code(stakk::jj::not_found),
        help("make sure jj is installed and available on your PATH")
    )]
    NotFound(std::io::Error),

    /// Could not determine the default branch.
    #[error("could not determine default branch; candidates: {candidates:?}")]
    #[diagnostic(
        code(stakk::jj::no_default_branch),
        help("ensure your repo has a trunk bookmark tracked from a remote")
    )]
    NoDefaultBranch { candidates: Vec<String> },
}

// Template for `jj bookmark list`: produces one JSON object per line.
const BOOKMARK_TEMPLATE: &str =
    r#""{\"name\":" ++ json(self.name()) ++ ",\"target\":" ++ json(self.normal_target()) ++ "}\n""#;

// Template for `jj bookmark list` when only names matter: one JSON string per
// line. Deliberately does not go through `BOOKMARK_TEMPLATE`, whose parser
// drops conflicted bookmarks (no normal target) — those names are still taken.
const BOOKMARK_NAME_TEMPLATE: &str = r#"json(self.name()) ++ "\n""#;

// Template for `jj log` when only commit ids matter: one JSON string per line.
pub(crate) const COMMIT_ID_TEMPLATE: &str = r#"json(commit_id) ++ "\n""#;

// Template for `jj log`: produces one JSON object per line with commit +
// bookmarks + shortest unique change ID prefix.
const LOG_TEMPLATE: &str = r#""{\"commit\":" ++ json(self) ++ ",\"local_bookmarks\":" ++ json(local_bookmarks) ++ ",\"immutable\":" ++ immutable ++ ",\"short_change_id\":\"" ++ change_id.shortest() ++ "\"}\n""#;

// Template for `jj log` when only a commit's remote bookmarks matter: one JSON
// array of `CommitRef`s per line.
const REMOTE_BOOKMARKS_TEMPLATE: &str = r#"json(remote_bookmarks) ++ "\n""#;

// Config key holding the definition of the `trunk()` revset alias.
const TRUNK_ALIAS_KEY: &str = r#"revset-aliases."trunk()""#;

/// jj's built-in `trunk()` candidates as `(bookmark, remote)`, in the order its
/// default `revset-aliases."trunk()"` lists them.
const BUILTIN_TRUNK_CANDIDATES: [(&str, &str); 6] = [
    ("main", "origin"),
    ("master", "origin"),
    ("trunk", "origin"),
    ("main", "upstream"),
    ("master", "upstream"),
    ("trunk", "upstream"),
];

/// Main interface for interacting with `jj`.
pub struct Jj<R: JjRunner> {
    runner: R,
}

impl<R: JjRunner> Jj<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Query and parse `jj --version`.
    ///
    /// Returns `Ok(None)` when jj ran but produced output we couldn't parse
    /// (e.g. an unusual dev build) — callers should stay silent in that case.
    /// Returns `Err` only when jj could not be run at all.
    pub async fn version(&self) -> Result<Option<JjVersion>, JjError> {
        let output = self.runner.run_jj(&["--version"]).await?;
        Ok(version::parse(&output))
    }

    /// List bookmarks matching the given revset.
    pub async fn get_my_bookmarks(&self, revset: &str) -> Result<Vec<Bookmark>, JjError> {
        let output = self
            .runner
            .run_jj(&["bookmark", "list", "-r", revset, "-T", BOOKMARK_TEMPLATE])
            .await?;

        parse_bookmarks(&output)
    }

    /// Every local bookmark name in the repo — the set of names
    /// `jj bookmark create` rejects.
    ///
    /// Unlike [`Jj::get_my_bookmarks`], this takes no revset: it includes
    /// trunk's bookmark, other people's bookmarks, bookmarks on immutable
    /// commits, conflicted bookmarks, and bookmarks deleted locally but still
    /// tracked on a remote. All of those collide on creation, and none of
    /// them are necessarily visible in the change graph.
    pub async fn get_local_bookmark_names(&self) -> Result<HashSet<String>, JjError> {
        let output = self
            .runner
            .run_jj(&["bookmark", "list", "-T", BOOKMARK_NAME_TEMPLATE])
            .await?;

        parse_bookmark_names(&output)
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

    /// Detect the default branch: the remote bookmark that `trunk()` names.
    ///
    /// `trunk()` evaluates to a commit, and other remote bookmarks can point
    /// at the same one, so the commit alone does not say which bookmark is
    /// meant. The alias definition does; see [`pick_default_branch`].
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
                REMOTE_BOOKMARKS_TEMPLATE,
            ])
            .await?;

        let refs: Vec<CommitRefData> = match output.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => serde_json::from_str(line).map_err(|e| JjError::ParseError {
                context: "trunk() remote bookmarks".to_string(),
                source: e,
            })?,
            None => return Err(JjError::NoDefaultBranch { candidates: vec![] }),
        };

        // `@git` entries mirror the backing git repository's refs; they are
        // not a remote stakk can open a PR against.
        let candidates: Vec<(&str, &str)> = refs
            .iter()
            .filter_map(|r| Some((r.name.as_str(), r.remote.as_deref()?)))
            .filter(|&(_, remote)| remote != "git")
            .collect();
        let no_default_branch = || JjError::NoDefaultBranch {
            candidates: refs
                .iter()
                .map(|r| match &r.remote {
                    Some(remote) => format!("{}@{remote}", r.name),
                    None => r.name.clone(),
                })
                .collect(),
        };
        if candidates.is_empty() {
            return Err(no_default_branch());
        }

        let trunk_alias = self
            .runner
            .run_jj(&["config", "get", TRUNK_ALIAS_KEY])
            .await?;
        pick_default_branch(&trunk_alias, &candidates)
            .map(str::to_string)
            .ok_or_else(no_default_branch)
    }

    /// Push a bookmark to a remote.
    pub async fn push_bookmark(&self, bookmark: &str, remote: &str) -> Result<(), JjError> {
        self.runner
            .run_jj(&["git", "push", "--remote", remote, "--bookmark", bookmark])
            .await?;
        Ok(())
    }

    /// Create a bookmark on a specific revision.
    pub async fn create_bookmark(&self, name: &str, revision: &str) -> Result<(), JjError> {
        self.runner
            .run_jj(&["bookmark", "create", name, "-r", revision])
            .await?;
        Ok(())
    }

    /// Delete a local bookmark.
    ///
    /// For a bookmark that was never pushed this is purely local. A bookmark
    /// with a tracked remote counterpart would have its deletion propagated
    /// on the next push, so callers pass only never-pushed names.
    pub async fn delete_bookmark(&self, name: &str) -> Result<(), JjError> {
        self.runner.run_jj(&["bookmark", "delete", name]).await?;
        Ok(())
    }

    /// Get head changes matching the given revset.
    ///
    /// These are the tips of branches that may not have bookmarks yet.
    /// Used to discover unbookmarked changes beyond the last bookmark.
    pub async fn get_heads(&self, revset: &str) -> Result<Vec<LogEntry>, JjError> {
        let output = self
            .runner
            .run_jj(&[
                "log",
                "-r",
                revset,
                "--no-graph",
                "--limit",
                "100",
                "-T",
                LOG_TEMPLATE,
            ])
            .await?;
        parse_log_entries(&output)
    }

    /// Commit ids of every commit in `revset`, in `jj log` order.
    ///
    /// The revset goes to jj verbatim, so anything `jj log -r` accepts
    /// resolves here. A revset jj rejects — an unknown symbol, an ambiguous
    /// id prefix, a syntax error — surfaces as [`JjError::CommandFailed`]
    /// carrying jj's message; a valid revset that selects nothing is
    /// `Ok(vec![])`.
    pub async fn resolve_revset(&self, revset: &str) -> Result<Vec<String>, JjError> {
        let output = self
            .runner
            .run_jj(&["log", "-r", revset, "--no-graph", "-T", COMMIT_ID_TEMPLATE])
            .await?;
        parse_commit_ids(&output)
    }

    /// Get the list of files changed by a specific commit.
    pub async fn get_diff_files(&self, commit_id: &str) -> Result<Vec<String>, JjError> {
        let output = self
            .runner
            .run_jj(&["diff", "-r", commit_id, "--name-only"])
            .await?;

        Ok(output
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
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
            });
        }
    }
    Ok(bookmarks)
}

/// Parse the output of [`BOOKMARK_NAME_TEMPLATE`]: one JSON string per line.
///
/// A bookmark can appear more than once (unsynced bookmarks emit a local and a
/// remote entry, deleted-but-tracked ones only remote entries); the set
/// collapses those.
fn parse_bookmark_names(output: &str) -> Result<HashSet<String>, JjError> {
    let mut names = HashSet::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let name: String = serde_json::from_str(line).map_err(|e| JjError::ParseError {
            context: "bookmark name list".to_string(),
            source: e,
        })?;
        names.insert(name);
    }
    Ok(names)
}

/// Parse the output of [`COMMIT_ID_TEMPLATE`]: one JSON string per line.
fn parse_commit_ids(output: &str) -> Result<Vec<String>, JjError> {
    let mut ids = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let id: String = serde_json::from_str(line).map_err(|e| JjError::ParseError {
            context: "commit id list".to_string(),
            source: e,
        })?;
        ids.push(id);
    }
    Ok(ids)
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
            committer: raw.commit.committer,
            local_bookmark_names: raw.local_bookmarks.iter().map(|b| b.name.clone()).collect(),
            immutable: raw.immutable,
            short_change_id: raw.short_change_id,
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

/// Choose which remote bookmark on the `trunk()` commit is the default branch.
///
/// `candidates` are the commit's remote bookmarks as `(name, remote)`, without
/// `@git`, in jj's listing order (by name). `trunk_alias` is the definition of
/// `revset-aliases."trunk()"`.
///
/// 1. An alias that is a single `<name>@<remote>` symbol names the bookmark
///    outright. `jj git clone` writes one when the remote's HEAD is known, and
///    it is how jj tells users to override `trunk()`. It comes first because
///    other bookmarks, built-in trunk names among them, can share the commit.
/// 2. Otherwise, the first candidate in jj's built-in `trunk()` order.
/// 3. Otherwise — a custom expression stakk cannot attribute to one bookmark —
///    the first candidate.
///
/// Returns `None` only when `candidates` is empty.
fn pick_default_branch<'a>(
    trunk_alias: &str,
    candidates: &[(&'a str, &'a str)],
) -> Option<&'a str> {
    // `rsplit_once`: a quoted bookmark name may itself contain `@`. Anything
    // that is not a lone symbol splits into pieces that match no candidate.
    if let Some((name, remote)) = trunk_alias.trim().rsplit_once('@') {
        let named = (unquote(name.trim()), unquote(remote.trim()));
        if let Some(&(name, _)) = candidates.iter().find(|&&c| c == named) {
            return Some(name);
        }
    }
    BUILTIN_TRUNK_CANDIDATES
        .iter()
        .find_map(|builtin| candidates.iter().find(|&c| c == builtin))
        .or_else(|| candidates.first())
        .map(|&(name, _)| name)
}

/// Strip one pair of matching quotes from a revset string literal.
///
/// Escape sequences inside a double-quoted literal are not interpreted; a name
/// that needs them matches no candidate, and `pick_default_branch` falls
/// through to its next rule.
fn unquote(s: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(quote).and_then(|s| s.strip_suffix(quote)) {
            return inner;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_bookmarks tests --

    #[test]
    fn parse_bookmarks_single() {
        let input = r#"{"name":"feature","target":{"commit_id":"abc123","parents":["def456"],"change_id":"xyz789","description":"my feature\n","author":{"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"},"committer":{"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"}}}"#;
        let bookmarks = parse_bookmarks(input).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "feature");
        assert_eq!(bookmarks[0].commit_id, "abc123");
        assert_eq!(bookmarks[0].change_id, "xyz789");
    }

    #[test]
    fn parse_bookmarks_multiple() {
        let input = concat!(
            r#"{"name":"a","target":{"commit_id":"111","parents":[],"change_id":"aaa","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#,
            "\n",
            r#"{"name":"b","target":{"commit_id":"222","parents":[],"change_id":"bbb","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#,
        );
        let bookmarks = parse_bookmarks(input).unwrap();
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].name, "a");
        assert_eq!(bookmarks[1].name, "b");
    }

    #[test]
    fn parse_bookmarks_conflicted_skipped() {
        let input = r#"{"name":"conflict","target":null}"#;
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
        let local = r#"{"name":"feat","target":{"commit_id":"new","parents":[],"change_id":"x1","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#;
        let remote = r#"{"name":"feat","target":{"commit_id":"old","parents":[],"change_id":"x2","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#;
        let input = format!("{local}\n{remote}");
        let bookmarks = parse_bookmarks(&input).unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "feat");
        assert_eq!(bookmarks[0].commit_id, "new");
    }

    // -- parse_bookmark_names tests --

    #[test]
    fn parse_bookmark_names_collects_and_dedupes() {
        // A deleted-but-tracked bookmark emits one line per remote entry, so
        // the same name can repeat.
        let input = "\"main\"\n\"feat\"\n\"feat\"\n";
        let names = parse_bookmark_names(input).unwrap();
        assert_eq!(
            names,
            HashSet::from(["main".to_string(), "feat".to_string()])
        );
    }

    #[test]
    fn parse_bookmark_names_keeps_conflicted_bookmarks() {
        // Conflicted bookmarks have no normal target and are dropped by
        // `parse_bookmarks`, but their names are still taken.
        let conflicted = r#"{"name":"conflict","target":null}"#;
        assert!(parse_bookmarks(conflicted).unwrap().is_empty());
        assert!(
            parse_bookmark_names("\"conflict\"\n")
                .unwrap()
                .contains("conflict")
        );
    }

    #[test]
    fn parse_bookmark_names_handles_quoting_and_empty_input() {
        assert!(parse_bookmark_names("").unwrap().is_empty());
        let names = parse_bookmark_names("\"weird\\\"name\"\n").unwrap();
        assert!(names.contains("weird\"name"));
    }

    #[test]
    fn parse_bookmark_names_rejects_malformed_output() {
        let err = parse_bookmark_names("not-json\n").unwrap_err();
        assert!(matches!(err, JjError::ParseError { .. }));
    }

    #[tokio::test]
    async fn get_local_bookmark_names_passes_no_revset() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "bookmark");
                assert_eq!(args[1], "list");
                // No `-r`: the reserved-name set must not be narrowed by any
                // revset.
                assert!(!args.contains(&"-r"), "unexpected revset: {args:?}");
                assert_eq!(args[2], "-T");
                Ok("\"main\"\n\"feat\"\n".to_string())
            },
        };
        let jj = Jj::new(runner);
        let names = jj.get_local_bookmark_names().await.unwrap();
        assert_eq!(
            names,
            HashSet::from(["main".to_string(), "feat".to_string()])
        );
    }

    // -- parse_log_entries tests --

    #[test]
    fn parse_log_entries_single() {
        let input = r#"{"commit":{"commit_id":"abc","parents":["def"],"change_id":"xyz","description":"desc\n","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[{"name":"feat","target":["abc"]}],"immutable":false,"short_change_id":"xyz"}"#;
        let entries = parse_log_entries(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit_id, "abc");
        assert_eq!(entries[0].local_bookmark_names, vec!["feat"]);
    }

    #[test]
    fn parse_log_entries_no_bookmarks() {
        let input = r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"immutable":false,"short_change_id":"xyz"}"#;
        let entries = parse_log_entries(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].local_bookmark_names.is_empty());
    }

    #[test]
    fn parse_log_entries_multiple() {
        let line1 = r#"{"commit":{"commit_id":"aaa","parents":[],"change_id":"x1","description":"first","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"immutable":false,"short_change_id":"x1"}"#;
        let line2 = r#"{"commit":{"commit_id":"bbb","parents":["aaa"],"change_id":"x2","description":"second","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"immutable":false,"short_change_id":"x2"}"#;
        let input = format!("{line1}\n{line2}");
        let entries = parse_log_entries(&input).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].commit_id, "aaa");
        assert_eq!(entries[1].commit_id, "bbb");
    }

    #[test]
    fn parse_log_entries_immutable_true() {
        let input = r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[{"name":"pinned","target":["abc"]}],"immutable":true,"short_change_id":"xyz"}"#;
        let entries = parse_log_entries(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].immutable);
        assert_eq!(entries[0].local_bookmark_names, vec!["pinned"]);
    }

    /// The `immutable` field is deliberately not serde-defaulted: output from
    /// a template that doesn't emit it must fail loudly, not parse as false.
    #[test]
    fn parse_log_entries_missing_immutable_is_error() {
        let input = r#"{"commit":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"short_change_id":"xyz"}"#;
        let result = parse_log_entries(input);
        assert!(matches!(result, Err(JjError::ParseError { .. })));
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
        fn run_jj(
            &self,
            args: &[&str],
        ) -> impl std::future::Future<Output = Result<String, JjError>> + Send {
            std::future::ready((self.handler)(args))
        }
    }

    #[tokio::test]
    async fn get_my_bookmarks_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "bookmark");
                assert_eq!(args[1], "list");
                assert_eq!(args[3], "custom-revset");
                Ok(r#"{"name":"my-feature","target":{"commit_id":"abc","parents":[],"change_id":"xyz","description":"feat","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}}}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let bookmarks = jj.get_my_bookmarks("custom-revset").await.unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "my-feature");
    }

    #[tokio::test]
    async fn resolve_revset_passes_the_revset_verbatim_and_keeps_order() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(
                    args,
                    [
                        "log",
                        "-r",
                        "trunk()..@",
                        "--no-graph",
                        "-T",
                        COMMIT_ID_TEMPLATE
                    ]
                );
                Ok("\"bbb222\"\n\"aaa111\"\n".to_string())
            },
        };
        let jj = Jj::new(runner);
        let ids = jj.resolve_revset("trunk()..@").await.unwrap();
        assert_eq!(ids, vec!["bbb222", "aaa111"]);
    }

    #[tokio::test]
    async fn resolve_revset_empty_selection_is_ok_and_empty() {
        let runner = MockJjRunner {
            handler: |_: &[&str]| Ok(String::new()),
        };
        let jj = Jj::new(runner);
        assert!(jj.resolve_revset("none()").await.unwrap().is_empty());
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

    // -- default branch tests --

    /// jj 0.46's built-in `revset-aliases."trunk()"`, as `jj config get`
    /// prints it.
    const BUILTIN_TRUNK_ALIAS: &str = r#"latest(
  remote_bookmarks(exact:"main", exact:"origin") |
  remote_bookmarks(exact:"master", exact:"origin") |
  remote_bookmarks(exact:"trunk", exact:"origin") |
  remote_bookmarks(exact:"main", exact:"upstream") |
  remote_bookmarks(exact:"master", exact:"upstream") |
  remote_bookmarks(exact:"trunk", exact:"upstream") |
  root()
)
"#;

    /// `REMOTE_BOOKMARKS_TEMPLATE` on a `trunk()` commit that carries `aaa`
    /// and `master`, both pushed to `origin` — captured from jj 0.46.
    const AAA_AND_MASTER_ON_TRUNK: &str = r#"[{"name":"aaa","remote":"git","target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"],"tracking_target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"]},{"name":"aaa","remote":"origin","target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"],"tracking_target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"]},{"name":"master","remote":"git","target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"],"tracking_target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"]},{"name":"master","remote":"origin","target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"],"tracking_target":["de1f8c71835f98a2ffadebcb26170d4a7cb825de"]}]
"#;

    fn default_branch_runner(
        remote_bookmarks: &'static str,
        trunk_alias: &'static str,
    ) -> MockJjRunner<impl Fn(&[&str]) -> Result<String, JjError> + Send + Sync> {
        MockJjRunner {
            handler: move |args: &[&str]| match args {
                [
                    "log",
                    "-r",
                    "trunk()",
                    "--no-graph",
                    "--limit",
                    "1",
                    "-T",
                    REMOTE_BOOKMARKS_TEMPLATE,
                ] => Ok(remote_bookmarks.to_string()),
                ["config", "get", TRUNK_ALIAS_KEY] => Ok(trunk_alias.to_string()),
                other => panic!("unexpected jj invocation: {other:?}"),
            },
        }
    }

    #[tokio::test]
    async fn get_default_branch_follows_the_clone_written_trunk_alias() {
        let jj = Jj::new(default_branch_runner(
            AAA_AND_MASTER_ON_TRUNK,
            "master@origin\n",
        ));
        assert_eq!(jj.get_default_branch().await.unwrap(), "master");
    }

    #[tokio::test]
    async fn get_default_branch_prefers_builtin_trunk_names_over_name_order() {
        let jj = Jj::new(default_branch_runner(
            AAA_AND_MASTER_ON_TRUNK,
            BUILTIN_TRUNK_ALIAS,
        ));
        assert_eq!(jj.get_default_branch().await.unwrap(), "master");
    }

    #[tokio::test]
    async fn get_default_branch_no_remote_bookmarks() {
        let jj = Jj::new(default_branch_runner("[]\n", "unreachable"));
        let result = jj.get_default_branch().await;
        assert!(matches!(
            result,
            Err(JjError::NoDefaultBranch { candidates }) if candidates.is_empty()
        ));
    }

    #[tokio::test]
    async fn get_default_branch_ignores_git_only_bookmarks() {
        let jj = Jj::new(default_branch_runner(
            r#"[{"name":"main","remote":"git","target":["abc"],"tracking_target":["abc"]}]"#,
            "unreachable",
        ));
        let result = jj.get_default_branch().await;
        assert!(matches!(
            result,
            Err(JjError::NoDefaultBranch { candidates }) if candidates == ["main@git"]
        ));
    }

    #[test]
    fn pick_default_branch_alias_wins_over_builtin_names_on_the_same_commit() {
        let candidates = [("develop", "origin"), ("main", "origin")];
        assert_eq!(
            pick_default_branch("develop@origin", &candidates),
            Some("develop")
        );
    }

    #[test]
    fn pick_default_branch_alias_must_match_the_remote_too() {
        let candidates = [("aaa", "origin"), ("main", "upstream")];
        assert_eq!(
            pick_default_branch("aaa@upstream", &candidates),
            Some("main")
        );
    }

    #[test]
    fn pick_default_branch_unquotes_alias_names() {
        let candidates = [("aaa", "origin"), ("rel@2", "origin")];
        assert_eq!(
            pick_default_branch(r#""rel@2"@'origin'"#, &candidates),
            Some("rel@2")
        );
    }

    #[test]
    fn pick_default_branch_builtin_order_puts_origin_before_upstream() {
        let candidates = [("master", "upstream"), ("trunk", "origin")];
        assert_eq!(
            pick_default_branch(BUILTIN_TRUNK_ALIAS, &candidates),
            Some("trunk")
        );
    }

    #[test]
    fn pick_default_branch_custom_expression_falls_back_to_the_first_candidate() {
        let candidates = [("dev", "origin"), ("rel", "origin")];
        assert_eq!(
            pick_default_branch("latest(rel@origin | dev@origin)", &candidates),
            Some("dev")
        );
    }

    #[test]
    fn pick_default_branch_without_candidates_is_none() {
        assert_eq!(pick_default_branch("main@origin", &[]), None);
    }

    #[tokio::test]
    async fn get_branch_changes_paginated_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "log");
                // Check the revset is constructed correctly
                let revset = args[2];
                assert!(revset.contains(".."));
                Ok(r#"{"commit":{"commit_id":"c1","parents":["c0"],"change_id":"ch1","description":"change 1","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"immutable":false,"short_change_id":"ch1"}"#.to_string())
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

    #[tokio::test]
    async fn get_heads_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "log");
                assert_eq!(args[2], "custom-heads-revset");
                Ok(r#"{"commit":{"commit_id":"h1","parents":["c_a"],"change_id":"ch_h1","description":"unbookmarked head","author":{"name":"A","email":"a@b.c","timestamp":"T"},"committer":{"name":"A","email":"a@b.c","timestamp":"T"}},"local_bookmarks":[],"immutable":false,"short_change_id":"ch_h"}"#.to_string())
            },
        };
        let jj = Jj::new(runner);
        let heads = jj.get_heads("custom-heads-revset").await.unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].commit_id, "h1");
        assert_eq!(heads[0].change_id, "ch_h1");
        assert!(heads[0].local_bookmark_names.is_empty());
    }

    #[tokio::test]
    async fn create_bookmark_integration() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args[0], "bookmark");
                assert_eq!(args[1], "create");
                assert_eq!(args[2], "stakk-abcdefghijkl");
                assert_eq!(args[3], "-r");
                assert_eq!(args[4], "abcdefghijklmnop");
                Ok(String::new())
            },
        };
        let jj = Jj::new(runner);
        jj.create_bookmark("stakk-abcdefghijkl", "abcdefghijklmnop")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn version_parses_plain() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                assert_eq!(args, ["--version"]);
                Ok("jj 0.42.0\n".to_string())
            },
        };
        let jj = Jj::new(runner);
        let version = jj.version().await.unwrap();
        assert_eq!(
            version,
            Some(JjVersion {
                major: 0,
                minor: 42,
                patch: 0
            })
        );
    }

    #[tokio::test]
    async fn version_dev_build_parses() {
        let runner = MockJjRunner {
            handler: |_args: &[&str]| {
                Ok("jj 0.42.0-b8f7c455170e3273897aaf94431f8ccfb1afa7ad\n".to_string())
            },
        };
        let jj = Jj::new(runner);
        let version = jj.version().await.unwrap();
        assert_eq!(
            version,
            Some(JjVersion {
                major: 0,
                minor: 42,
                patch: 0
            })
        );
    }

    #[tokio::test]
    async fn version_garbage_is_none() {
        let runner = MockJjRunner {
            handler: |_args: &[&str]| Ok("not a version\n".to_string()),
        };
        let jj = Jj::new(runner);
        assert_eq!(jj.version().await.unwrap(), None);
    }

    #[tokio::test]
    async fn version_runner_error_propagates() {
        let runner = MockJjRunner {
            handler: |_args: &[&str]| Err(JjError::NotFound(std::io::Error::other("boom"))),
        };
        let jj = Jj::new(runner);
        assert!(jj.version().await.is_err());
    }
}
