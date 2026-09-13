//! Wire shapes of the Forgejo API responses stakk reads.
//!
//! Each struct carries exactly the fields the forge consumes and is
//! deliberately *not* serde-defaulted (the `LogEntryRaw` rule): a field
//! Forgejo stops sending fails the parse loudly instead of quietly turning
//! into an empty string or a zero. Everything else in a response is ignored
//! by serde. The structs are `Deserialize` only — stakk never writes these
//! shapes; request bodies are built as `serde_json::Value`s in `super`.

use serde::Deserialize;

use super::super::Comment;
use super::super::PullRequest;

/// A pull request as returned by `GET/POST /repos/{o}/{r}/pulls`.
#[derive(Debug, Deserialize)]
pub struct PullRequestDto {
    pub number: u64,
    pub html_url: String,
    pub title: String,
    /// Forgejo returns `""` for an empty body in practice, never `null`, but
    /// the swagger allows it.
    pub body: Option<String>,
    pub head: RefDto,
    pub base: RefDto,
}

/// The branch side of a pull request (`head` or `base`).
#[derive(Debug, Deserialize)]
pub struct RefDto {
    /// The branch name.
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// An issue comment. Pull requests are issues in the Gitea data model, so
/// PR comments come from the `/issues/` routes.
#[derive(Debug, Deserialize)]
pub struct CommentDto {
    pub id: u64,
    pub body: String,
}

/// The one field of a Forgejo error body that is read. Lenient by nature: it
/// is only consulted when a non-2xx body happens to parse, and the raw body
/// is the message otherwise.
#[derive(Debug, Deserialize)]
pub struct ErrorBody {
    pub message: String,
}

impl From<PullRequestDto> for PullRequest {
    fn from(dto: PullRequestDto) -> Self {
        Self {
            number: dto.number,
            html_url: dto.html_url,
            title: dto.title,
            base_ref: dto.base.ref_name,
            body: dto.body,
        }
    }
}

impl From<CommentDto> for Comment {
    fn from(dto: CommentDto) -> Self {
        Self {
            id: dto.id,
            body: dto.body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `POST /repos/stakk/probe/pulls` on Forgejo 16.0.4, verbatim.
    const PULL_REQUEST_CREATE: &str = include_str!("fixtures/pull_request_create.json");
    /// `GET /repos/stakk/probe/pulls?state=open&page=1&limit=50`, verbatim.
    const PULLS_LIST_OPEN: &str = include_str!("fixtures/pulls_list_open.json");
    /// `GET /repos/stakk/probe/issues/1/comments`, verbatim.
    const ISSUE_COMMENTS: &str = include_str!("fixtures/issue_comments.json");
    /// A 404 body, verbatim.
    const ERROR_NOT_FOUND: &str = include_str!("fixtures/error_not_found.json");

    #[test]
    fn pull_request_reads_the_nested_head_and_base_refs() {
        let pr: PullRequestDto = serde_json::from_str(PULL_REQUEST_CREATE).unwrap();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.html_url, "http://localhost:3999/stakk/probe/pulls/1");
        assert_eq!(pr.title, "feat a");
        assert_eq!(pr.body.as_deref(), Some("body a"));
        // The branch name sits under `ref`, next to `label` and `sha`; the
        // rename is what makes `ref_name` read it.
        assert_eq!(pr.head.ref_name, "feat-a");
        assert_eq!(pr.base.ref_name, "main");
    }

    #[test]
    fn pull_request_converts_base_ref_from_the_nested_base() {
        let dto: PullRequestDto = serde_json::from_str(PULL_REQUEST_CREATE).unwrap();
        let pr = PullRequest::from(dto);
        assert_eq!(pr.number, 1);
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.title, "feat a");
        assert_eq!(pr.body.as_deref(), Some("body a"));
        assert_eq!(pr.html_url, "http://localhost:3999/stakk/probe/pulls/1");
    }

    #[test]
    fn open_listing_is_newest_first() {
        let pulls: Vec<PullRequestDto> = serde_json::from_str(PULLS_LIST_OPEN).unwrap();
        let heads: Vec<(u64, &str, &str)> = pulls
            .iter()
            .map(|pr| {
                (
                    pr.number,
                    pr.head.ref_name.as_str(),
                    pr.base.ref_name.as_str(),
                )
            })
            .collect();
        assert_eq!(heads, vec![(2, "feat-b", "feat-a"), (1, "feat-a", "main")]);
    }

    #[test]
    fn comment_reads_id_and_body() {
        let comments: Vec<CommentDto> = serde_json::from_str(ISSUE_COMMENTS).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, 6);
        assert_eq!(comments[0].body, "hello2");
        let comment = Comment::from(
            serde_json::from_str::<Vec<CommentDto>>(ISSUE_COMMENTS)
                .unwrap()
                .remove(0),
        );
        assert_eq!(comment.id, 6);
        assert_eq!(comment.body, "hello2");
    }

    #[test]
    fn error_body_reads_the_message() {
        let body: ErrorBody = serde_json::from_str(ERROR_NOT_FOUND).unwrap();
        assert_eq!(body.message, "The target couldn't be found.");
    }

    /// Not serde-defaulted: an API-shape change fails the parse rather than
    /// handing the forge a PR numbered 0 or a branch named "".
    #[test]
    fn a_missing_required_field_is_a_parse_error() {
        let without_number = PULL_REQUEST_CREATE.replacen(r#""number":1,"#, "", 1);
        assert!(serde_json::from_str::<PullRequestDto>(&without_number).is_err());

        let without_head_ref = PULL_REQUEST_CREATE.replacen(r#""ref":"feat-a","#, "", 1);
        assert!(serde_json::from_str::<PullRequestDto>(&without_head_ref).is_err());

        let without_comment_body = ISSUE_COMMENTS.replacen(r#""body":"hello2","#, "", 1);
        assert!(serde_json::from_str::<Vec<CommentDto>>(&without_comment_body).is_err());
    }

    #[test]
    fn a_null_body_parses_as_none() {
        let with_null_body =
            PULL_REQUEST_CREATE.replacen(r#""body":"body a","#, r#""body":null,"#, 1);
        let pr: PullRequestDto = serde_json::from_str(&with_null_body).unwrap();
        assert_eq!(pr.body, None);
    }
}
