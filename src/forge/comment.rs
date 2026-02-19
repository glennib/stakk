//! Stack comment formatting and parsing.
//!
//! Comments include base64-encoded metadata on the first line so that
//! future runs can identify and update the same comment idempotently.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde::Serialize;

use super::Comment;

/// Prefix for the metadata HTML comment.
const COMMENT_DATA_PREFIX: &str = "<!--- JACK_STACK: ";
const COMMENT_DATA_POSTFIX: &str = " --->";
/// Unicode left arrow used to mark the current PR in the stack list.
const STACK_COMMENT_THIS_PR: &str = "\u{2190} this PR";
const STACK_COMMENT_FOOTER: &str = "*Created with [jack](https://github.com/glennib/jack)*";

/// Metadata embedded in stack comments as base64-encoded JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackCommentData {
    pub version: u32,
    pub stack: Vec<StackEntry>,
}

/// One entry in the stack comment metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackEntry {
    /// The jj bookmark name.
    pub bookmark_name: String,
    /// Full URL to the pull request.
    pub pr_url: String,
    /// PR number.
    pub pr_number: u64,
}

/// Format a stack comment body for a specific PR in the stack.
///
/// `current_index` is the index into `data.stack` for the PR this comment
/// will be posted on.
pub fn format_stack_comment(data: &StackCommentData, current_index: usize) -> String {
    let encoded = BASE64.encode(serde_json::to_string(data).expect("serialization cannot fail"));

    let plural = if data.stack.len() == 1 { "" } else { "s" };
    let mut body = format!(
        "{COMMENT_DATA_PREFIX}{encoded}{COMMENT_DATA_POSTFIX}\nThis PR is part of a stack of {} \
         bookmark{plural}:\n\n1. `trunk()`\n",
        data.stack.len(),
    );

    for (i, entry) in data.stack.iter().enumerate() {
        if i == current_index {
            body.push_str(&format!(
                "1. **{} {STACK_COMMENT_THIS_PR}**\n",
                entry.pr_url,
            ));
        } else {
            body.push_str(&format!("1. {}\n", entry.pr_url));
        }
    }

    body.push_str(&format!("\n---\n{STACK_COMMENT_FOOTER}"));
    body
}

/// Find the existing stack comment among a list of comments.
///
/// Detects by the `JACK_STACK` metadata prefix on the first line.
pub fn find_stack_comment(comments: &[Comment]) -> Option<&Comment> {
    comments
        .iter()
        .find(|c| c.body.contains(COMMENT_DATA_PREFIX))
}

/// Parse stack comment metadata from a comment body.
///
/// Returns `None` if the comment does not contain valid metadata.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used in submit milestone for reading existing stack data"
    )
)]
pub fn parse_stack_comment(body: &str) -> Option<StackCommentData> {
    let first_line = body.lines().next()?;
    let start = first_line.find(COMMENT_DATA_PREFIX)? + COMMENT_DATA_PREFIX.len();
    let end = first_line[start..].find(COMMENT_DATA_POSTFIX)? + start;
    let encoded = &first_line[start..end];
    let decoded = BASE64.decode(encoded).ok()?;
    let json_str = std::str::from_utf8(&decoded).ok()?;
    serde_json::from_str(json_str).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data() -> StackCommentData {
        StackCommentData {
            version: 0,
            stack: vec![
                StackEntry {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://github.com/owner/repo/pull/1".to_string(),
                    pr_number: 1,
                },
                StackEntry {
                    bookmark_name: "feat-b".to_string(),
                    pr_url: "https://github.com/owner/repo/pull/2".to_string(),
                    pr_number: 2,
                },
            ],
        }
    }

    #[test]
    fn format_and_parse_roundtrip() {
        let data = sample_data();
        let body = format_stack_comment(&data, 0);
        let parsed = parse_stack_comment(&body).unwrap();
        assert_eq!(parsed, data);
    }

    #[test]
    fn format_highlights_current_pr() {
        let data = sample_data();
        let body = format_stack_comment(&data, 1);
        // Second PR should be highlighted.
        assert!(body.contains("**https://github.com/owner/repo/pull/2 \u{2190} this PR**"));
        // First PR should not be bold.
        assert!(!body.contains("**https://github.com/owner/repo/pull/1"));
    }

    #[test]
    fn format_includes_trunk() {
        let body = format_stack_comment(&sample_data(), 0);
        assert!(body.contains("`trunk()`"));
    }

    #[test]
    fn find_stack_comment_matches() {
        let comments = vec![
            Comment {
                id: 1,
                body: "Some unrelated comment".to_string(),
            },
            Comment {
                id: 2,
                body: format_stack_comment(&sample_data(), 0),
            },
        ];
        let found = find_stack_comment(&comments);
        assert_eq!(found.unwrap().id, 2);
    }

    #[test]
    fn find_stack_comment_none_when_absent() {
        let comments = vec![Comment {
            id: 1,
            body: "Nothing here".to_string(),
        }];
        assert!(find_stack_comment(&comments).is_none());
    }

    #[test]
    fn parse_with_different_body_text() {
        // Parse metadata even when the body text around it differs.
        let data = sample_data();
        let encoded = BASE64.encode(serde_json::to_string(&data).unwrap());
        let body = format!(
            "{COMMENT_DATA_PREFIX}{encoded}{COMMENT_DATA_POSTFIX}\nSome different body \
             text\n\n---\n*Some other footer*"
        );
        let parsed = parse_stack_comment(&body).unwrap();
        assert_eq!(parsed, data);
    }

    #[test]
    fn parse_invalid_base64_returns_none() {
        let body = format!("{COMMENT_DATA_PREFIX}not-valid-base64!!!{COMMENT_DATA_POSTFIX}\nstuff");
        assert!(parse_stack_comment(&body).is_none());
    }

    #[test]
    fn parse_no_metadata_returns_none() {
        assert!(parse_stack_comment("just a regular comment").is_none());
    }

    #[test]
    fn format_single_bookmark_no_plural() {
        let data = StackCommentData {
            version: 0,
            stack: vec![StackEntry {
                bookmark_name: "solo".to_string(),
                pr_url: "https://github.com/o/r/pull/1".to_string(),
                pr_number: 1,
            }],
        };
        let body = format_stack_comment(&data, 0);
        assert!(body.contains("1 bookmark:"));
        assert!(!body.contains("bookmarks:"));
    }

    #[test]
    fn format_multiple_bookmarks_plural() {
        let body = format_stack_comment(&sample_data(), 0);
        assert!(body.contains("2 bookmarks:"));
    }

    #[test]
    fn format_includes_footer() {
        let body = format_stack_comment(&sample_data(), 0);
        assert!(body.contains(STACK_COMMENT_FOOTER));
    }
}
