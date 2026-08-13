//! Serde structs for `jj` JSON output.

use serde::Deserialize;

/// Commit data from `jj`'s `json(self)` in log context.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitData {
    pub commit_id: String,
    pub parents: Vec<String>,
    pub change_id: String,
    pub description: String,
    pub author: Signature,
    pub committer: Signature,
}

/// Author/committer signature.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub timestamp: String,
}

/// `CommitRef` serialization from `jj` (used in bookmark arrays on log
/// entries).
///
/// jj also emits a `tracking_target` key on remote refs — an array whose
/// elements are null when the tracking target is absent (e.g. after the
/// tracked commit was rewritten). Nothing here declares it, and the struct
/// has no `#[serde(deny_unknown_fields)]`, so it is silently ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitRefData {
    pub name: String,
    /// Target commit IDs. Nothing reads them, but the field has no
    /// `#[serde(default)]` on purpose: it keeps jj's `CommitRef` shape a hard
    /// parse contract, so a template change that drops `target` fails loudly
    /// as a `ParseError` — the same rule as `LogEntryRaw::immutable`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "required-by-serde field: its absence must fail the parse, and \
                      `deserialize_commit_ref_local` pins that it arrives"
        )
    )]
    pub target: Vec<String>,
    #[serde(default)]
    pub remote: Option<String>,
}

/// Raw log entry: commit + bookmark refs from the log template.
#[derive(Debug, Clone, Deserialize)]
pub struct LogEntryRaw {
    pub commit: CommitData,
    pub local_bookmarks: Vec<CommitRefData>,
    pub remote_bookmarks: Vec<CommitRefData>,
    /// Whether jj considers the commit immutable. Deliberately not
    /// serde-defaulted: a template/parser mismatch must fail loudly.
    pub immutable: bool,
    /// Shortest unique change ID prefix (from `change_id.shortest(4)`).
    pub short_change_id: String,
}

/// Raw bookmark entry from `jj bookmark list` with explicit field template.
#[derive(Debug, Clone, Deserialize)]
pub struct BookmarkEntryRaw {
    pub name: String,
    pub synced: bool,
    /// `None` if the bookmark is conflicted (no normal target).
    pub target: Option<CommitData>,
}

/// Processed bookmark for public API.
#[derive(Debug, Clone)]
pub struct Bookmark {
    pub name: String,
    pub commit_id: String,
    /// Change ID of the bookmark's target. `BOOKMARK_TEMPLATE` has no
    /// change-ID field of its own; this comes from the nested `CommitData` in
    /// `BookmarkEntryRaw::target`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "`parse_bookmarks_single` pins that the change ID is taken from the target \
                      commit, not the bookmark entry"
        )
    )]
    pub change_id: String,
    /// Whether the local bookmark matches its remote tracking target.
    ///
    /// This is what gives `BookmarkEntryRaw::synced` — a field with no
    /// `#[serde(default)]`, so `BOOKMARK_TEMPLATE` dropping it would fail the
    /// parse loudly — its production reader.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "`parse_bookmarks_single` pins that the template's `synced` field reaches \
                      the parsed bookmark"
        )
    )]
    pub synced: bool,
}

/// Processed log entry for public API.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub commit_id: String,
    pub change_id: String,
    pub description: String,
    pub parents: Vec<String>,
    pub author: Signature,
    pub committer: Signature,
    pub local_bookmark_names: Vec<String>,
    pub remote_bookmark_names: Vec<String>,
    /// Whether jj considers the commit immutable.
    pub immutable: bool,
    /// Shortest unique change ID prefix (from jj).
    pub short_change_id: String,
}

/// A git remote parsed from `jj git remote list`.
#[derive(Debug, Clone)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_commit_data() {
        let json = r#"{
            "commit_id": "4fcf70e0abc",
            "parents": ["f601ec4def"],
            "change_id": "xqwwpttp123",
            "description": "feat: add something\n",
            "author": {
                "name": "Glenn",
                "email": "glenn@example.com",
                "timestamp": "2026-02-19T19:47:54+01:00"
            },
            "committer": {
                "name": "Glenn",
                "email": "glenn@example.com",
                "timestamp": "2026-02-19T19:47:54+01:00"
            }
        }"#;
        let commit: CommitData = serde_json::from_str(json).unwrap();
        assert_eq!(commit.commit_id, "4fcf70e0abc");
        assert_eq!(commit.parents, vec!["f601ec4def"]);
        assert_eq!(commit.change_id, "xqwwpttp123");
        assert_eq!(commit.author.name, "Glenn");
    }

    #[test]
    fn deserialize_commit_ref_local() {
        let json = r#"{"name":"main","target":["4fcf70e0abc"]}"#;
        let cr: CommitRefData = serde_json::from_str(json).unwrap();
        assert_eq!(cr.name, "main");
        assert_eq!(cr.target, vec!["4fcf70e0abc"]);
        assert!(cr.remote.is_none());
    }

    #[test]
    fn deserialize_commit_ref_remote() {
        let json = r#"{
            "name": "main",
            "remote": "origin",
            "target": ["4fcf70e0abc"],
            "tracking_target": ["4fcf70e0abc"]
        }"#;
        let cr: CommitRefData = serde_json::from_str(json).unwrap();
        assert_eq!(cr.name, "main");
        assert_eq!(cr.remote.as_deref(), Some("origin"));
    }

    #[test]
    fn deserialize_log_entry_raw() {
        let json = r#"{
            "commit": {
                "commit_id": "abc123",
                "parents": ["def456"],
                "change_id": "xyz789",
                "description": "some change\n",
                "author": {"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"},
                "committer": {"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"}
            },
            "local_bookmarks": [
                {"name":"feature","target":["abc123"]}
            ],
            "remote_bookmarks": [
                {"name":"feature","remote":"origin","target":["abc123"],"tracking_target":["abc123"]}
            ],
            "immutable": false,
            "short_change_id": "xyz7"
        }"#;
        let entry: LogEntryRaw = serde_json::from_str(json).unwrap();
        assert_eq!(entry.commit.commit_id, "abc123");
        assert!(!entry.immutable);
        assert_eq!(entry.local_bookmarks.len(), 1);
        assert_eq!(entry.local_bookmarks[0].name, "feature");
        assert_eq!(entry.remote_bookmarks.len(), 1);
        assert_eq!(entry.remote_bookmarks[0].remote.as_deref(), Some("origin"));
    }

    #[test]
    fn deserialize_bookmark_entry_raw() {
        let json = r#"{
            "name": "feature",
            "synced": false,
            "target": {
                "commit_id": "abc123",
                "parents": ["def456"],
                "change_id": "xyz789",
                "description": "my feature\n",
                "author": {"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"},
                "committer": {"name":"A","email":"a@b.c","timestamp":"2026-01-01T00:00:00Z"}
            }
        }"#;
        let entry: BookmarkEntryRaw = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name, "feature");
        assert!(!entry.synced);
        assert!(entry.target.is_some());
        assert_eq!(entry.target.unwrap().commit_id, "abc123");
    }

    #[test]
    fn deserialize_bookmark_entry_conflicted() {
        let json = r#"{"name":"conflict","synced":false,"target":null}"#;
        let entry: BookmarkEntryRaw = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name, "conflict");
        assert!(entry.target.is_none());
    }
}
