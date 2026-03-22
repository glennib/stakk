//! Custom bookmark name generation via external shell command.
//!
//! Provides validation, caching, and async command invocation for custom
//! bookmark names. The command receives a JSON segment description on stdin
//! and returns a single bookmark name on stdout.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use super::bookmark_widget::BookmarkRow;

/// Errors from the bookmark name generation command.
#[derive(Debug, Error, Diagnostic)]
pub enum BookmarkGenError {
    /// The command exited with a non-zero status.
    #[error("bookmark command failed (exit code {exit_code}): {stderr}")]
    #[diagnostic(
        code(stakk::bookmark_command::failed),
        help("check your bookmark command for errors")
    )]
    CommandFailed { exit_code: i32, stderr: String },

    /// The command was not found.
    #[error("bookmark command not found: {command}")]
    #[diagnostic(
        code(stakk::bookmark_command::not_found),
        help("check that the command exists and is on your PATH")
    )]
    NotFound {
        command: String,
        #[source]
        source: std::io::Error,
    },

    /// The command produced no output.
    #[error("bookmark command produced empty output: {command}")]
    #[diagnostic(
        code(stakk::bookmark_command::empty_output),
        help("the command must print a single bookmark name to stdout")
    )]
    EmptyOutput { command: String },

    /// The generated name is invalid.
    #[error("invalid bookmark name {name:?}: {reason}")]
    #[diagnostic(
        code(stakk::bookmark_command::invalid_name),
        help("bookmark names must be valid git refs")
    )]
    InvalidName { name: String, reason: String },

    /// An I/O error.
    #[error("bookmark command I/O error: {0}")]
    #[diagnostic(code(stakk::bookmark_command::io))]
    Io(#[from] std::io::Error),
}

/// JSON protocol: top-level object sent to the command on stdin.
#[derive(Debug, Serialize)]
pub(super) struct SegmentInput {
    rules: RulesInput,
    commits: Vec<CommitInput>,
}

/// Validation rules sent to the command.
#[derive(Debug, Serialize)]
struct RulesInput {
    max_length: usize,
    disallowed_chars: String,
}

/// A commit in the segment, sent to the command on stdin.
#[derive(Debug, Serialize)]
struct CommitInput {
    commit_id: String,
    change_id: String,
    short_change_id: String,
    description: String,
    author: AuthorInput,
    files: Vec<String>,
}

/// Author info for a commit.
#[derive(Debug, Serialize)]
struct AuthorInput {
    name: String,
    email: String,
    timestamp: String,
}

/// Maximum bookmark name length in bytes.
pub(super) const MAX_BOOKMARK_LENGTH: usize = 255;

/// Characters disallowed in bookmark names.
pub(super) const DISALLOWED_CHARS: &str = " ~^:?*[\\";

/// Timeout for in-flight cache entries before they can be retried.
pub const COMPUTING_TIMEOUT: Duration = Duration::from_secs(60);

/// A cache entry for a custom bookmark name.
#[derive(Debug, Clone)]
pub enum CacheEntry {
    /// A background task is computing this name.
    Computing { since: Instant },
    /// The name has been computed and validated.
    Computed(String),
}

impl CacheEntry {
    /// Returns `true` if this is a `Computing` entry that has exceeded the
    /// timeout.
    pub fn is_expired(&self) -> bool {
        match self {
            CacheEntry::Computing { since } => since.elapsed() > COMPUTING_TIMEOUT,
            CacheEntry::Computed(_) => false,
        }
    }
}

/// Cache for custom bookmark names, keyed by ordered commit IDs.
pub type BookmarkNameCache = HashMap<Vec<String>, CacheEntry>;

/// Generate the default `stakk-<change_id[:12]>` bookmark name.
pub fn default_bookmark_name(change_id: &str) -> String {
    let prefix = if change_id.len() >= 12 {
        &change_id[..12]
    } else {
        change_id
    };
    format!("stakk-{prefix}")
}

/// Validate a bookmark name against git ref rules.
pub fn validate_bookmark_name(name: &str) -> Result<(), BookmarkGenError> {
    if name.is_empty() {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: "name is empty".to_string(),
        });
    }
    if name.len() > MAX_BOOKMARK_LENGTH {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: format!("exceeds maximum length of {MAX_BOOKMARK_LENGTH} bytes"),
        });
    }
    if name.starts_with('-') || name.starts_with('.') {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: format!("cannot start with {:?}", &name[..1]),
        });
    }
    if name.ends_with('.') {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: "cannot end with '.'".to_string(),
        });
    }
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "git ref rule, not a file extension"
    )]
    if name.ends_with(".lock") {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: "cannot end with '.lock'".to_string(),
        });
    }
    if name.contains("..") {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: "cannot contain '..'".to_string(),
        });
    }
    if name.contains("@{") {
        return Err(BookmarkGenError::InvalidName {
            name: name.to_string(),
            reason: "cannot contain '@{'".to_string(),
        });
    }
    for ch in name.chars() {
        if ch.is_ascii_control() || DISALLOWED_CHARS.contains(ch) {
            return Err(BookmarkGenError::InvalidName {
                name: name.to_string(),
                reason: format!("contains disallowed character {ch:?}"),
            });
        }
    }
    Ok(())
}

/// Build the cache key from a set of bookmark rows (ordered commit IDs).
pub fn cache_key(rows: &[&BookmarkRow]) -> Vec<String> {
    rows.iter().map(|r| r.commit_id.clone()).collect()
}

/// Generate a custom bookmark name by invoking the external command.
///
/// Results are cached by the ordered commit IDs of the segment.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "app.rs calls build_segment_input + run_command directly for async spawning"
    )
)]
pub async fn generate_custom_name(
    command: &str,
    rows: &[&BookmarkRow],
    cache: &mut BookmarkNameCache,
) -> Result<String, BookmarkGenError> {
    let key = cache_key(rows);
    if let Some(CacheEntry::Computed(name)) = cache.get(&key) {
        return Ok(name.clone());
    }

    let input = build_segment_input(rows);
    let json = serde_json::to_string(&input).expect("SegmentInput is always serializable");

    let name = run_command(command, &json).await?;
    validate_bookmark_name(&name)?;

    cache.insert(key, CacheEntry::Computed(name.clone()));
    Ok(name)
}

/// Build the JSON input struct from bookmark rows. Exposed for `app.rs` to
/// serialize before spawning a background task.
pub(super) fn build_segment_input(rows: &[&BookmarkRow]) -> SegmentInput {
    SegmentInput {
        rules: RulesInput {
            max_length: MAX_BOOKMARK_LENGTH,
            disallowed_chars: DISALLOWED_CHARS.to_string(),
        },
        commits: rows
            .iter()
            .map(|row| CommitInput {
                commit_id: row.commit_id.clone(),
                change_id: row.change_id.clone(),
                short_change_id: row.short_change_id.clone(),
                description: row.description.clone(),
                author: AuthorInput {
                    name: row.author.name.clone(),
                    email: row.author.email.clone(),
                    timestamp: row.author.timestamp.clone(),
                },
                files: row.files.clone(),
            })
            .collect(),
    }
}

/// Run the shell command with the given JSON on stdin. Exposed for `app.rs`
/// to spawn as a background task.
pub(super) async fn run_command(
    command: &str,
    stdin_data: &str,
) -> Result<String, BookmarkGenError> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut child = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", command])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    } else {
        Command::new("sh")
            .args(["-c", command])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    }
    .map_err(|e| BookmarkGenError::NotFound {
        command: command.to_string(),
        source: e,
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore BrokenPipe — the command may exit before reading all of
        // stdin (e.g. `echo foo` ignores stdin entirely).
        match stdin.write_all(stdin_data.as_bytes()).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return Err(e.into()),
        }
        // Drop to close stdin so the child can finish.
    }

    let output = child.wait_with_output().await?;

    if !output.status.success() {
        return Err(BookmarkGenError::CommandFailed {
            exit_code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        return Err(BookmarkGenError::EmptyOutput {
            command: command.to_string(),
        });
    }

    Ok(name)
}

/// Collect rows forming the dynamic segment starting at `row_idx`.
///
/// A dynamic segment runs from the given row downward (toward trunk) until
/// hitting another checked (non-`Unchecked`) row or reaching the trunk row.
/// The returned rows are in trunk-to-tip order (oldest first).
pub fn dynamic_segment_commits(rows: &[BookmarkRow], row_idx: usize) -> Vec<&BookmarkRow> {
    use super::bookmark_widget::RowState;

    let mut segment = vec![&rows[row_idx]];

    // Walk downward (toward trunk).
    for i in (0..row_idx).rev() {
        let row = &rows[i];
        if row.is_trunk {
            break;
        }
        if row.state != RowState::Unchecked {
            break;
        }
        segment.push(row);
    }

    // Reverse to get trunk-to-tip order.
    segment.reverse();
    segment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jj::types::Signature;
    use crate::select::bookmark_widget::RowState;

    #[test]
    fn default_name_long_id() {
        assert_eq!(
            default_bookmark_name("abcdefghijklmnop"),
            "stakk-abcdefghijkl"
        );
    }

    #[test]
    fn default_name_short_id() {
        assert_eq!(default_bookmark_name("short"), "stakk-short");
    }

    #[test]
    fn valid_names_pass() {
        for name in ["feature", "my-branch", "fix/thing", "a.b", "CAPS"] {
            validate_bookmark_name(name).unwrap();
        }
    }

    #[test]
    fn invalid_names_rejected() {
        let cases = [
            ("", "empty"),
            ("-leading", "start with"),
            (".leading", "start with"),
            ("trailing.", "end with '.'"),
            ("foo.lock", "end with '.lock'"),
            ("has..dots", "contain '..'"),
            ("has@{ref", "contain '@{'"),
            ("has space", "disallowed character"),
            ("has~tilde", "disallowed character"),
            ("has^caret", "disallowed character"),
            ("has:colon", "disallowed character"),
            ("has?question", "disallowed character"),
            ("has*star", "disallowed character"),
            ("has[bracket", "disallowed character"),
            ("has\\backslash", "disallowed character"),
        ];
        for (name, expected_reason) in cases {
            let err = validate_bookmark_name(name).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(expected_reason),
                "name={name:?}: expected reason containing {expected_reason:?}, got {msg:?}"
            );
        }
    }

    #[test]
    fn too_long_name_rejected() {
        let name = "a".repeat(256);
        let err = validate_bookmark_name(&name).unwrap_err();
        assert!(err.to_string().contains("maximum length"));
    }

    #[test]
    fn cache_hit_and_miss() {
        let mut cache = BookmarkNameCache::new();
        let key = vec!["c1".to_string(), "c2".to_string()];
        cache.insert(key.clone(), CacheEntry::Computed("cached-name".to_string()));
        assert!(
            matches!(cache.get(&key), Some(CacheEntry::Computed(name)) if name == "cached-name")
        );

        let miss_key = vec!["c3".to_string()];
        assert!(!cache.contains_key(&miss_key));
    }

    #[test]
    fn computing_entry_overwrites_on_generate() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];
        let key = cache_key(&rows);

        // Insert a Computing entry.
        cache.insert(
            key.clone(),
            CacheEntry::Computing {
                since: Instant::now(),
            },
        );

        // generate_custom_name should run the command and overwrite with
        // Computed.
        let name = rt
            .block_on(generate_custom_name("echo my-branch", &rows, &mut cache))
            .unwrap();
        assert_eq!(name, "my-branch");
        assert!(matches!(cache.get(&key), Some(CacheEntry::Computed(n)) if n == "my-branch"));
    }

    #[test]
    fn expired_computing_entry_is_treated_as_absent() {
        let mut cache = BookmarkNameCache::new();
        let key = vec!["c1".to_string()];

        // Insert an expired Computing entry.
        cache.insert(
            key.clone(),
            CacheEntry::Computing {
                since: Instant::now().checked_sub(Duration::from_secs(61)).unwrap(),
            },
        );

        assert!(cache.get(&key).unwrap().is_expired());

        // A non-expired entry should not be expired.
        cache.insert(
            key.clone(),
            CacheEntry::Computing {
                since: Instant::now(),
            },
        );
        assert!(!cache.get(&key).unwrap().is_expired());
    }

    fn make_row(commit_id: &str, change_id: &str, state: RowState, is_trunk: bool) -> BookmarkRow {
        BookmarkRow {
            change_id: change_id.to_string(),
            short_change_id: change_id[..4.min(change_id.len())].to_string(),
            commit_id: commit_id.to_string(),
            summary: "test".to_string(),
            description: "test".to_string(),
            existing_bookmarks: vec![],
            state,
            generated_name: Some(default_bookmark_name(change_id)),
            is_trunk,
            author: Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            custom_name: None,
            tfidf_name: None,
            user_input_name: None,
            existing_bookmark_idx: 0,
            has_bookmark_command: false,
        }
    }

    #[test]
    fn dynamic_segment_single_checked_row() {
        let rows = vec![
            make_row("c0", "ch0", RowState::Unchecked, true),
            make_row("c1", "ch1", RowState::Unchecked, false),
            make_row("c2", "ch2", RowState::UseGenerated, false),
        ];
        let segment = dynamic_segment_commits(&rows, 2);
        assert_eq!(segment.len(), 2);
        assert_eq!(segment[0].commit_id, "c1");
        assert_eq!(segment[1].commit_id, "c2");
    }

    #[test]
    fn dynamic_segment_stops_at_checked_neighbor() {
        let rows = vec![
            make_row("c0", "ch0", RowState::Unchecked, true),
            make_row("c1", "ch1", RowState::UseExisting(0), false),
            make_row("c2", "ch2", RowState::Unchecked, false),
            make_row("c3", "ch3", RowState::UseGenerated, false),
        ];
        let segment = dynamic_segment_commits(&rows, 3);
        assert_eq!(segment.len(), 2);
        assert_eq!(segment[0].commit_id, "c2");
        assert_eq!(segment[1].commit_id, "c3");
    }

    #[test]
    fn dynamic_segment_stops_at_trunk() {
        let rows = vec![
            make_row("c0", "ch0", RowState::Unchecked, true),
            make_row("c1", "ch1", RowState::UseGenerated, false),
        ];
        let segment = dynamic_segment_commits(&rows, 1);
        assert_eq!(segment.len(), 1);
        assert_eq!(segment[0].commit_id, "c1");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_returns_expected_output() {
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];
        let name = generate_custom_name("echo my-branch", &rows, &mut cache)
            .await
            .unwrap();
        assert_eq!(name, "my-branch");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_failure_returns_error() {
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];
        let err = generate_custom_name("false", &rows, &mut cache)
            .await
            .unwrap_err();
        assert!(matches!(err, BookmarkGenError::CommandFailed { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_command_returns_not_found() {
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];
        // sh -c with a non-existent command returns exit 127, not NotFound.
        // NotFound only fires if sh itself is missing, which we can't easily
        // test. Instead verify the CommandFailed path for missing programs.
        let err = generate_custom_name("nonexistent_command_xyz_12345", &rows, &mut cache)
            .await
            .unwrap_err();
        assert!(matches!(err, BookmarkGenError::CommandFailed { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn empty_output_returns_error() {
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];
        let err = generate_custom_name("echo -n ''", &rows, &mut cache)
            .await
            .unwrap_err();
        assert!(matches!(err, BookmarkGenError::EmptyOutput { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cached_result_reused() {
        let mut cache = BookmarkNameCache::new();
        let row = make_row("c1", "ch1", RowState::UseGenerated, false);
        let rows: Vec<&BookmarkRow> = vec![&row];

        // First call populates cache.
        let name1 = generate_custom_name("echo cached-name", &rows, &mut cache)
            .await
            .unwrap();
        assert_eq!(name1, "cached-name");

        // Second call with a different command still returns cached value.
        let name2 = generate_custom_name("echo different-name", &rows, &mut cache)
            .await
            .unwrap();
        assert_eq!(name2, "cached-name");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_receives_json_stdin() {
        use std::io::Write;

        let mut cache = BookmarkNameCache::new();
        let mut row = make_row("c1", "ch1_full_change_id", RowState::UseGenerated, false);
        row.description = "add login page".to_string();
        row.files = vec!["src/login.rs".to_string()];
        let rows: Vec<&BookmarkRow> = vec![&row];

        // Use a command that reads stdin and outputs a fixed name.
        let tmpdir = std::env::temp_dir();
        let script_path = tmpdir.join("stakk_test_stdin.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            // Save stdin to a file for inspection, output a valid name.
            let capture_path = tmpdir.join("stakk_test_stdin_capture.json");
            writeln!(f, "cat > {}", capture_path.display()).unwrap();
            writeln!(f, "echo valid-name").unwrap();
        }
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let name =
            generate_custom_name(&format!("sh {}", script_path.display()), &rows, &mut cache)
                .await
                .unwrap();
        assert_eq!(name, "valid-name");

        // Verify the JSON that was sent.
        let capture_path = tmpdir.join("stakk_test_stdin_capture.json");
        let captured = std::fs::read_to_string(&capture_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(parsed["rules"]["max_length"], 255);
        assert!(
            parsed["rules"]["disallowed_chars"]
                .as_str()
                .unwrap()
                .contains('~')
        );
        assert_eq!(parsed["commits"][0]["description"], "add login page");
        assert_eq!(parsed["commits"][0]["files"][0], "src/login.rs");

        // Clean up.
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&capture_path);
    }
}
