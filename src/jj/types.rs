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
    #[expect(dead_code, reason = "deserialized for completeness, used later")]
    pub committer: Signature,
}

/// Author/committer signature.
#[derive(Debug, Clone, Deserialize)]
pub struct Signature {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "deserialized for completeness, used later")
    )]
    pub name: String,
    #[expect(dead_code, reason = "deserialized for completeness, used later")]
    pub email: String,
    #[expect(dead_code, reason = "deserialized for completeness, used later")]
    pub timestamp: String,
}

/// CommitRef serialization from `jj` (used in bookmark arrays on log entries).
#[derive(Debug, Clone, Deserialize)]
pub struct CommitRefData {
    pub name: String,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "deserialized for completeness, used later")
    )]
    pub target: Vec<String>,
    #[serde(default)]
    pub remote: Option<String>,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "deserialized for completeness, used later")
    )]
    #[serde(default)]
    pub tracking_target: Option<Vec<String>>,
}

/// Raw log entry: commit + bookmark refs from the log template.
#[derive(Debug, Clone, Deserialize)]
pub struct LogEntryRaw {
    pub commit: CommitData,
    pub local_bookmarks: Vec<CommitRefData>,
    pub remote_bookmarks: Vec<CommitRefData>,
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
    pub change_id: String,
    pub synced: bool,
}

/// Processed log entry for public API.
#[derive(Debug, Clone)]
pub struct LogEntry {
    #[cfg_attr(not(test), expect(dead_code, reason = "used in graph milestone"))]
    pub commit_id: String,
    #[expect(dead_code, reason = "used in graph milestone")]
    pub change_id: String,
    #[expect(dead_code, reason = "used in graph milestone")]
    pub description: String,
    #[expect(dead_code, reason = "used in graph milestone")]
    pub parents: Vec<String>,
    #[expect(dead_code, reason = "used in graph milestone")]
    pub author: Signature,
    #[cfg_attr(not(test), expect(dead_code, reason = "used in graph milestone"))]
    pub local_bookmark_names: Vec<String>,
    pub remote_bookmark_names: Vec<String>,
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
        assert!(cr.tracking_target.is_none());
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
        assert_eq!(
            cr.tracking_target.as_deref(),
            Some(vec!["4fcf70e0abc".to_string()].as_slice())
        );
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
            ]
        }"#;
        let entry: LogEntryRaw = serde_json::from_str(json).unwrap();
        assert_eq!(entry.commit.commit_id, "abc123");
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
