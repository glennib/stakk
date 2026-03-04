//! Stack comment formatting and parsing.
//!
//! Comments include base64-encoded metadata on the first line so that
//! future runs can identify and update the same comment idempotently.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use minijinja::Environment;
use serde::Deserialize;
use serde::Serialize;

use super::Comment;
use crate::submit::SubmitError;

/// Prefix for the metadata HTML comment.
const COMMENT_DATA_PREFIX: &str = "<!--- STAKK_STACK: ";
const COMMENT_DATA_POSTFIX: &str = " --->";

const DEFAULT_TEMPLATE: &str = include_str!("default_comment.md.jinja");

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

/// Template rendering context for a full stack comment.
#[derive(Debug, Clone, Serialize)]
pub struct StackCommentContext {
    pub stack: Vec<StackEntryContext>,
    pub stack_size: usize,
    pub default_branch: String,
    pub current_bookmark: String,
    pub stakk_url: String,
}

/// Template rendering context for a single entry in the stack.
#[derive(Debug, Clone, Serialize)]
pub struct StackEntryContext {
    pub bookmark_name: String,
    pub pr_url: String,
    pub pr_number: u64,
    pub title: String,
    pub base: String,
    pub is_draft: bool,
    pub position: usize,
    pub is_current: bool,
}

/// Build a minijinja environment with the stack comment template loaded.
///
/// If `custom_template` is `Some`, it is used instead of the built-in
/// default.
pub fn build_comment_env(
    custom_template: Option<&str>,
) -> Result<Environment<'static>, SubmitError> {
    let mut env = Environment::new();
    let source = match custom_template {
        Some(s) => s.to_string(),
        None => DEFAULT_TEMPLATE.to_string(),
    };
    env.add_template("stack_comment", Box::leak(source.into_boxed_str()))
        .map_err(|e| SubmitError::TemplateRenderFailed {
            message: format!("failed to compile template: {e}"),
        })?;
    Ok(env)
}

/// Format a stack comment body for a specific PR in the stack.
///
/// The metadata line (`<!--- STAKK_STACK: ... --->`) is always prepended
/// programmatically and is NOT part of the template.
pub fn format_stack_comment(
    data: &StackCommentData,
    context: &StackCommentContext,
    template: &minijinja::Template<'_, '_>,
) -> Result<String, SubmitError> {
    let encoded = BASE64.encode(serde_json::to_string(data).expect("serialization cannot fail"));
    let metadata_line = format!("{COMMENT_DATA_PREFIX}{encoded}{COMMENT_DATA_POSTFIX}");

    let rendered = template
        .render(context)
        .map_err(|e| SubmitError::TemplateRenderFailed {
            message: e.to_string(),
        })?;

    Ok(format!("{metadata_line}\n{rendered}"))
}

/// Find the existing stack comment among a list of comments.
///
/// Detects by the `STAKK_STACK` metadata prefix on the first line.
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
        reason = "needed when submission reads existing stack data (e.g. detecting merged PRs)"
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

    fn sample_context(current_index: usize) -> StackCommentContext {
        let entries = vec![
            StackEntryContext {
                bookmark_name: "feat-a".to_string(),
                pr_url: "https://github.com/owner/repo/pull/1".to_string(),
                pr_number: 1,
                title: "feature a".to_string(),
                base: "main".to_string(),
                is_draft: false,
                position: 1,
                is_current: current_index == 0,
            },
            StackEntryContext {
                bookmark_name: "feat-b".to_string(),
                pr_url: "https://github.com/owner/repo/pull/2".to_string(),
                pr_number: 2,
                title: "feature b".to_string(),
                base: "feat-a".to_string(),
                is_draft: false,
                position: 2,
                is_current: current_index == 1,
            },
        ];
        StackCommentContext {
            stack_size: entries.len(),
            current_bookmark: entries[current_index].bookmark_name.clone(),
            stack: entries,
            default_branch: "main".to_string(),
            stakk_url: "https://github.com/glennib/stakk".to_string(),
        }
    }

    fn default_env() -> Environment<'static> {
        build_comment_env(None).unwrap()
    }

    #[test]
    fn format_and_parse_roundtrip() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        let parsed = parse_stack_comment(&body).unwrap();
        assert_eq!(parsed, data);
    }

    #[test]
    fn format_highlights_current_pr() {
        let data = sample_data();
        let ctx = sample_context(1);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        // Second PR should be highlighted with pointing finger.
        assert!(
            body.contains("\u{1f448}"),
            "expected pointing finger emoji in body: {body}"
        );
    }

    #[test]
    fn format_includes_default_branch() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        assert!(body.contains("`main`"));
    }

    #[test]
    fn find_stack_comment_matches() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let comments = vec![
            Comment {
                id: 1,
                body: "Some unrelated comment".to_string(),
            },
            Comment {
                id: 2,
                body: format_stack_comment(&data, &ctx, &tmpl).unwrap(),
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
    fn format_single_entry_numbered_list() {
        let data = StackCommentData {
            version: 0,
            stack: vec![StackEntry {
                bookmark_name: "solo".to_string(),
                pr_url: "https://github.com/o/r/pull/1".to_string(),
                pr_number: 1,
            }],
        };
        let ctx = StackCommentContext {
            stack: vec![StackEntryContext {
                bookmark_name: "solo".to_string(),
                pr_url: "https://github.com/o/r/pull/1".to_string(),
                pr_number: 1,
                title: "solo feature".to_string(),
                base: "main".to_string(),
                is_draft: false,
                position: 1,
                is_current: true,
            }],
            stack_size: 1,
            default_branch: "main".to_string(),
            current_bookmark: "solo".to_string(),
            stakk_url: "https://github.com/glennib/stakk".to_string(),
        };
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        assert!(
            body.contains("1. https://github.com/o/r/pull/1"),
            "expected numbered list entry: {body}"
        );
    }

    #[test]
    fn format_includes_footer() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        assert!(body.contains("stakk"));
    }

    #[test]
    fn format_header_mentions_merges_into() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        assert!(
            body.contains("merges into `main`"),
            "expected merge target in header: {body}"
        );
    }

    #[test]
    fn custom_template_renders() {
        let data = sample_data();
        let ctx = sample_context(0);
        let custom = "Custom: {{ stack_size }} PRs for {{ current_bookmark }}";
        let env = build_comment_env(Some(custom)).unwrap();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        assert!(body.contains("Custom: 2 PRs for feat-a"));
    }

    #[test]
    fn invalid_template_returns_error() {
        let result = build_comment_env(Some("{{ unclosed"));
        assert!(result.is_err());
    }

    #[test]
    fn format_renders_numbered_entries() {
        let data = sample_data();
        let ctx = sample_context(0);
        let env = default_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let body = format_stack_comment(&data, &ctx, &tmpl).unwrap();
        // Should contain numbered list entries with PR URLs
        assert!(
            body.contains("1. https://github.com/owner/repo/pull/1"),
            "expected entry 1: {body}"
        );
        assert!(
            body.contains("2. https://github.com/owner/repo/pull/2"),
            "expected entry 2: {body}"
        );
    }
}
