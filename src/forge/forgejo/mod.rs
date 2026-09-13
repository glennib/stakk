//! Forgejo implementation of the Forge trait.
//!
//! Forgejo serves the Gitea REST API under `/api/v1` on the same host as the
//! web UI. Every request is built here as an `http::Request<String>` and sent
//! through a [`Transport`], so the forge never touches a socket and its tests
//! observe the exact wire shape. Pull requests are issues in the Gitea data
//! model, which is why comments live under `/issues/`.
//!
//! Forgejo has no native stacked pull requests; the four stack methods say
//! so without a request (see the note on them below).

pub mod transport;
pub mod types;

use std::future::Future;

use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::AUTHORIZATION;
use http::header::CONTENT_TYPE;
use serde::de::DeserializeOwned;

use self::transport::ReqwestTransport;
use self::transport::Transport;
use self::types::CommentDto;
use self::types::ErrorBody;
use self::types::PullRequestDto;
use super::Comment;
use super::CreatePrParams;
use super::Forge;
use super::ForgeError;
use super::ForgeStack;
use super::PullRequest;

/// Items requested per page of a listing.
///
/// 50 is `MAX_RESPONSE_ITEMS` on a default instance. The listing loops stop
/// on a page *shorter than requested*, never on an empty page alone, so an
/// instance configured with a lower cap — which answers fewer items per page
/// than asked for — still terminates after its first page.
const PAGE_LIMIT: usize = 50;

/// Forgejo implementation of the `Forge` trait.
pub struct ForgejoForge<T: Transport> {
    transport: T,
    /// API root without a trailing slash, e.g. `http://localhost:3000/api/v1`.
    api_base: String,
    token: String,
    owner: String,
    repo: String,
}

impl ForgejoForge<ReqwestTransport> {
    /// Create a `ForgejoForge` over the real HTTP transport.
    ///
    /// `api_base_uri` is the instance's API root, `{scheme}://{host}/api/v1`
    /// — a plain string rather than a parsed remote so this module stays
    /// independent of `jj::remote`.
    pub fn new(
        token: &str,
        owner: String,
        repo: String,
        api_base_uri: &str,
    ) -> Result<Self, ForgeError> {
        let transport = ReqwestTransport::new().map_err(|e| ForgeError::Api {
            message: format!("failed to create Forgejo client: {e}"),
            source: Box::new(e),
        })?;
        Ok(Self::with_transport(
            transport,
            token,
            owner,
            repo,
            api_base_uri,
        ))
    }
}

impl<T: Transport> ForgejoForge<T> {
    /// Create a `ForgejoForge` over any transport — the seam the tests use.
    pub fn with_transport(
        transport: T,
        token: &str,
        owner: String,
        repo: String,
        api_base_uri: &str,
    ) -> Self {
        Self {
            transport,
            api_base: api_base_uri.trim_end_matches('/').to_string(),
            token: token.to_string(),
            owner,
            repo,
        }
    }

    /// A route under this repository, e.g. `/repos/{owner}/{repo}/pulls`.
    fn repo_route(&self, tail: &str) -> String {
        format!("/repos/{}/{}{tail}", self.owner, self.repo)
    }

    /// Build a request against the API root. Every request carries the token
    /// and asks for JSON; a payload adds its content type.
    fn build_request(
        &self,
        method: Method,
        route: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<http::Request<String>, ForgeError> {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("{}{route}", self.api_base))
            .header(AUTHORIZATION, format!("token {}", self.token))
            .header(ACCEPT, "application/json");

        let body = match payload {
            Some(value) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                serde_json::to_string(value).map_err(|e| ForgeError::Api {
                    message: format!("failed to serialize Forgejo request body: {e}"),
                    source: Box::new(e),
                })?
            }
            None => String::new(),
        };

        builder.body(body).map_err(|e| ForgeError::Api {
            message: format!("failed to build Forgejo request: {e}"),
            source: Box::new(e),
        })
    }

    /// Send a request and return the body of a 2xx response. A non-2xx
    /// status maps through [`map_forgejo_error`]; a transport failure is an
    /// [`ForgeError::Api`] carrying it as the source.
    async fn send(
        &self,
        method: Method,
        route: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<String, ForgeError> {
        let request = self.build_request(method, route, payload)?;
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|e| ForgeError::Api {
                message: format!("Forgejo request failed: {e}"),
                source: Box::new(e),
            })?;
        let (parts, body) = response.into_parts();
        if !parts.status.is_success() {
            return Err(map_forgejo_error(parts.status, &body));
        }
        Ok(body)
    }

    /// [`Self::send`], then parse the body.
    async fn send_json<D: DeserializeOwned>(
        &self,
        method: Method,
        route: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<D, ForgeError> {
        let body = self.send(method, route, payload).await?;
        serde_json::from_str(&body).map_err(|e| ForgeError::Api {
            message: format!("failed to parse Forgejo response: {e}"),
            source: Box::new(e),
        })
    }
}

impl<T: Transport> Forge for ForgejoForge<T> {
    async fn find_pr_for_branch(&self, head: &str) -> Result<Option<PullRequest>, ForgeError> {
        // The list route has no head filter, and the `/pulls/{base}/{head}`
        // lookup needs the base, which the caller does not have — so the
        // open listing is walked and filtered here, stopping at the first
        // match.
        let mut page = 1;
        loop {
            let route =
                self.repo_route(&format!("/pulls?state=open&page={page}&limit={PAGE_LIMIT}"));
            let pulls: Vec<PullRequestDto> = self.send_json(Method::GET, &route, None).await?;
            let count = pulls.len();
            if let Some(pr) = pulls.into_iter().find(|pr| pr.head.ref_name == head) {
                return Ok(Some(pr.into()));
            }
            if count < PAGE_LIMIT {
                return Ok(None);
            }
            page += 1;
        }
    }

    async fn create_pr(&self, params: CreatePrParams) -> Result<PullRequest, ForgeError> {
        // `draft` is not sent: Forgejo has no draft flag on this route — a
        // draft there is a `WIP:` title prefix — and `CreatePrParams::draft`
        // is always `false` today.
        let CreatePrParams {
            title,
            head,
            base,
            body,
            draft: _,
        } = params;
        let mut payload = serde_json::json!({
            "title": title,
            "head": head,
            "base": base,
        });
        if let Some(body) = body {
            payload["body"] = serde_json::Value::String(body);
        }
        let pr: PullRequestDto = self
            .send_json(Method::POST, &self.repo_route("/pulls"), Some(&payload))
            .await?;
        Ok(pr.into())
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<(), ForgeError> {
        let payload = serde_json::json!({ "base": new_base });
        self.send(
            Method::PATCH,
            &self.repo_route(&format!("/pulls/{pr_number}")),
            Some(&payload),
        )
        .await?;
        Ok(())
    }

    async fn update_pr_title(&self, pr_number: u64, title: &str) -> Result<(), ForgeError> {
        let payload = serde_json::json!({ "title": title });
        self.send(
            Method::PATCH,
            &self.repo_route(&format!("/pulls/{pr_number}")),
            Some(&payload),
        )
        .await?;
        Ok(())
    }

    async fn list_comments(&self, pr_number: u64) -> Result<Vec<Comment>, ForgeError> {
        let mut comments = Vec::new();
        let mut page = 1;
        loop {
            let route = self.repo_route(&format!(
                "/issues/{pr_number}/comments?page={page}&limit={PAGE_LIMIT}"
            ));
            let batch: Vec<CommentDto> = self.send_json(Method::GET, &route, None).await?;
            let count = batch.len();
            comments.extend(batch.into_iter().map(Comment::from));
            if count < PAGE_LIMIT {
                return Ok(comments);
            }
            page += 1;
        }
    }

    async fn create_comment(&self, pr_number: u64, body: &str) -> Result<Comment, ForgeError> {
        let payload = serde_json::json!({ "body": body });
        let comment: CommentDto = self
            .send_json(
                Method::POST,
                &self.repo_route(&format!("/issues/{pr_number}/comments")),
                Some(&payload),
            )
            .await?;
        Ok(comment.into())
    }

    async fn update_comment(&self, comment_id: u64, body: &str) -> Result<(), ForgeError> {
        let payload = serde_json::json!({ "body": body });
        self.send(
            Method::PATCH,
            &self.repo_route(&format!("/issues/comments/{comment_id}")),
            Some(&payload),
        )
        .await?;
        Ok(())
    }

    async fn update_pr_body(&self, pr_number: u64, body: &str) -> Result<(), ForgeError> {
        let payload = serde_json::json!({ "body": body });
        self.send(
            Method::PATCH,
            &self.repo_route(&format!("/pulls/{pr_number}")),
            Some(&payload),
        )
        .await?;
        Ok(())
    }

    async fn delete_comment(&self, comment_id: u64) -> Result<(), ForgeError> {
        // Answers 204 with an empty body.
        self.send(
            Method::DELETE,
            &self.repo_route(&format!("/issues/comments/{comment_id}")),
            None,
        )
        .await?;
        Ok(())
    }

    // Forgejo has no native stacked pull requests, and the four stack
    // methods say so without sending anything. stakk never probes for the
    // feature: the reconcile step's own outcome is the answer, and
    // `StacksUnavailable` is its "definitively not offered here" reading —
    // the same one a 404 from GitHub's stacks routes produces. That single
    // answer is what makes `--native-stacks auto` resolve the auto placements
    // to writing stack comments, `none` stay silent, and `on` fail naming the
    // cause instead of a 404 that never happened. `add_to_stack` and
    // `unstack` are unreachable in practice — the reconcile asks
    // `get_stacks_for_pr` first — and answer the same for consistency.

    fn get_stacks_for_pr(
        &self,
        _pr_number: u64,
    ) -> impl Future<Output = Result<Vec<ForgeStack>, ForgeError>> + Send {
        std::future::ready(Err(stacks_unavailable()))
    }

    fn create_stack(
        &self,
        _pr_numbers: &[u64],
    ) -> impl Future<Output = Result<ForgeStack, ForgeError>> + Send {
        std::future::ready(Err(stacks_unavailable()))
    }

    fn add_to_stack(
        &self,
        _stack_number: u64,
        _pr_numbers: &[u64],
    ) -> impl Future<Output = Result<ForgeStack, ForgeError>> + Send {
        std::future::ready(Err(stacks_unavailable()))
    }

    fn unstack(&self, _stack_number: u64) -> impl Future<Output = Result<(), ForgeError>> + Send {
        std::future::ready(Err(stacks_unavailable()))
    }
}

/// The one answer every stack method gives on Forgejo.
fn stacks_unavailable() -> ForgeError {
    ForgeError::StacksUnavailable {
        message: "Forgejo has no native stacked pull requests".into(),
        source: "Forgejo offers no stacks API".into(),
    }
}

/// Map a non-2xx Forgejo response to a [`ForgeError`].
///
/// 401 and 403 are authentication failures; everything else is an API error
/// carrying the status and Forgejo's `message` when the body parses as one,
/// the raw body otherwise.
fn map_forgejo_error(status: StatusCode, body: &str) -> ForgeError {
    let message = error_message(status, body);
    let source: Box<dyn std::error::Error + Send + Sync> =
        format!("Forgejo answered {status}: {message}").into();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            ForgeError::AuthFailed { message, source }
        }
        _ => ForgeError::Api {
            message: format!("{status}: {message}"),
            source,
        },
    }
}

/// Forgejo's `message` field when the body parses as an error body, else the
/// raw body text, else the status alone.
fn error_message(status: StatusCode, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<ErrorBody>(body) {
        return parsed.message;
    }
    let text = body.trim();
    if text.is_empty() {
        format!("HTTP {status}")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {

    use super::transport::testing::FailingTransport;
    use super::transport::testing::MockTransport;
    use super::transport::testing::response;
    use super::*;

    const PULL_REQUEST_CREATE: &str = include_str!("fixtures/pull_request_create.json");
    const PULLS_LIST_OPEN: &str = include_str!("fixtures/pulls_list_open.json");
    const ISSUE_COMMENTS: &str = include_str!("fixtures/issue_comments.json");
    const ERROR_NOT_FOUND: &str = include_str!("fixtures/error_not_found.json");

    const API_BASE: &str = "http://localhost:3999/api/v1";
    const TOKEN: &str = "s3cret";

    fn forge(responses: Vec<http::Response<String>>) -> ForgejoForge<MockTransport> {
        ForgejoForge::with_transport(
            MockTransport::new(responses),
            TOKEN,
            "stakk".into(),
            "probe".into(),
            API_BASE,
        )
    }

    /// What a test asserts about one recorded request.
    struct Expected<'a> {
        method: Method,
        /// Path and query below the API root.
        route: &'a str,
        /// The JSON payload, or `None` for a body-less request.
        body: Option<serde_json::Value>,
    }

    fn assert_requests(forge: &ForgejoForge<MockTransport>, expected: &[Expected<'_>]) {
        let requests = forge.transport.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            expected.len(),
            "unexpected number of requests: {:?}",
            requests
                .iter()
                .map(|r| format!("{} {}", r.method(), r.uri()))
                .collect::<Vec<_>>()
        );
        for (request, want) in requests.iter().zip(expected) {
            assert_eq!(request.method(), want.method);
            assert_eq!(
                request.uri().to_string(),
                format!("{API_BASE}{}", want.route)
            );
            assert_eq!(
                request.headers().get(AUTHORIZATION).unwrap(),
                &format!("token {TOKEN}")
            );
            assert_eq!(request.headers().get(ACCEPT).unwrap(), "application/json");
            if let Some(json) = &want.body {
                assert_eq!(
                    request.headers().get(CONTENT_TYPE).unwrap(),
                    "application/json"
                );
                let sent: serde_json::Value = serde_json::from_str(request.body()).unwrap();
                assert_eq!(&sent, json);
            } else {
                assert!(request.headers().get(CONTENT_TYPE).is_none());
                assert!(request.body().is_empty());
            }
        }
    }

    /// `count` open PRs shaped like the captured one, numbered from
    /// `first_number` and each on its own head branch `{head_prefix}-{n}`.
    fn page_of_pulls(count: usize, first_number: u64, head_prefix: &str) -> String {
        let template: serde_json::Value = serde_json::from_str(PULL_REQUEST_CREATE).unwrap();
        let pulls: Vec<serde_json::Value> = (0..count)
            .map(|i| {
                let mut pr = template.clone();
                let number = first_number + i as u64;
                pr["number"] = number.into();
                pr["head"]["ref"] = format!("{head_prefix}-{number}").into();
                pr
            })
            .collect();
        serde_json::to_string(&pulls).unwrap()
    }

    /// `count` comments shaped like the captured one, with ids from
    /// `first_id`.
    fn page_of_comments(count: usize, first_id: u64) -> String {
        let template: serde_json::Value = serde_json::from_str(ISSUE_COMMENTS).unwrap();
        let comments: Vec<serde_json::Value> = (0..count)
            .map(|i| {
                let mut comment = template[0].clone();
                comment["id"] = (first_id + i as u64).into();
                comment
            })
            .collect();
        serde_json::to_string(&comments).unwrap()
    }

    #[tokio::test]
    async fn find_pr_for_branch_filters_the_open_listing_by_head() {
        let forge = forge(vec![response(200, PULLS_LIST_OPEN)]);
        let pr = forge
            .find_pr_for_branch("feat-a")
            .await
            .unwrap()
            .expect("feat-a is in the listing");
        assert_eq!(pr.number, 1);
        assert_eq!(pr.base_ref, "main");
        assert_requests(
            &forge,
            &[Expected {
                method: Method::GET,
                route: "/repos/stakk/probe/pulls?state=open&page=1&limit=50",
                body: None,
            }],
        );
    }

    /// A first page shorter than the limit is the whole listing: one request
    /// and no match is `None`.
    #[tokio::test]
    async fn find_pr_for_branch_returns_none_after_one_short_page() {
        let forge = forge(vec![response(200, PULLS_LIST_OPEN)]);
        assert!(forge.find_pr_for_branch("nope").await.unwrap().is_none());
        assert_eq!(forge.transport.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn find_pr_for_branch_walks_a_full_page_then_a_short_one() {
        let forge = forge(vec![
            response(200, &page_of_pulls(PAGE_LIMIT, 100, "other")),
            response(200, PULLS_LIST_OPEN),
        ]);
        let pr = forge
            .find_pr_for_branch("feat-b")
            .await
            .unwrap()
            .expect("feat-b is on the second page");
        assert_eq!(pr.number, 2);
        assert_requests(
            &forge,
            &[
                Expected {
                    method: Method::GET,
                    route: "/repos/stakk/probe/pulls?state=open&page=1&limit=50",
                    body: None,
                },
                Expected {
                    method: Method::GET,
                    route: "/repos/stakk/probe/pulls?state=open&page=2&limit=50",
                    body: None,
                },
            ],
        );
    }

    /// The loop ends on the first page shorter than the limit, whether or not
    /// it is empty: a full page followed by an exhausted listing takes two
    /// requests, and the second can be empty.
    #[tokio::test]
    async fn find_pr_for_branch_ends_on_an_exhausted_listing() {
        let forge = forge(vec![
            response(200, &page_of_pulls(PAGE_LIMIT, 100, "other")),
            response(200, "[]"),
        ]);
        assert!(forge.find_pr_for_branch("nope").await.unwrap().is_none());
        assert_eq!(forge.transport.requests.lock().unwrap().len(), 2);
    }

    /// A match on a full page ends the walk: the next page is never asked
    /// for.
    #[tokio::test]
    async fn find_pr_for_branch_stops_as_soon_as_the_head_is_found() {
        let forge = forge(vec![
            response(200, &page_of_pulls(PAGE_LIMIT, 100, "other")),
            response(200, PULLS_LIST_OPEN),
        ]);
        let pr = forge
            .find_pr_for_branch("other-110")
            .await
            .unwrap()
            .expect("other-110 is on the first, full page");
        assert_eq!(pr.number, 110);
        assert_eq!(forge.transport.requests.lock().unwrap().len(), 1);
        assert_eq!(forge.transport.remaining_responses(), 1);
    }

    #[tokio::test]
    async fn create_pr_posts_title_head_base_and_body() {
        let forge = forge(vec![response(201, PULL_REQUEST_CREATE)]);
        let pr = forge
            .create_pr(CreatePrParams {
                title: "feat a".into(),
                head: "feat-a".into(),
                base: "main".into(),
                body: Some("body a".into()),
                draft: false,
            })
            .await
            .unwrap();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.html_url, "http://localhost:3999/stakk/probe/pulls/1");
        assert_eq!(pr.base_ref, "main");
        assert_requests(
            &forge,
            &[Expected {
                method: Method::POST,
                route: "/repos/stakk/probe/pulls",
                body: Some(serde_json::json!({
                    "title": "feat a",
                    "head": "feat-a",
                    "base": "main",
                    "body": "body a",
                })),
            }],
        );
    }

    #[tokio::test]
    async fn create_pr_omits_the_body_key_when_there_is_none() {
        let forge = forge(vec![response(201, PULL_REQUEST_CREATE)]);
        forge
            .create_pr(CreatePrParams {
                title: "feat a".into(),
                head: "feat-a".into(),
                base: "main".into(),
                body: None,
                draft: false,
            })
            .await
            .unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::POST,
                route: "/repos/stakk/probe/pulls",
                body: Some(serde_json::json!({
                    "title": "feat a",
                    "head": "feat-a",
                    "base": "main",
                })),
            }],
        );
    }

    #[tokio::test]
    async fn update_pr_base_patches_the_single_field() {
        let forge = forge(vec![response(201, PULL_REQUEST_CREATE)]);
        forge.update_pr_base(7, "feat-a").await.unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::PATCH,
                route: "/repos/stakk/probe/pulls/7",
                body: Some(serde_json::json!({ "base": "feat-a" })),
            }],
        );
    }

    #[tokio::test]
    async fn update_pr_title_patches_the_single_field() {
        let forge = forge(vec![response(201, PULL_REQUEST_CREATE)]);
        forge.update_pr_title(7, "new title").await.unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::PATCH,
                route: "/repos/stakk/probe/pulls/7",
                body: Some(serde_json::json!({ "title": "new title" })),
            }],
        );
    }

    #[tokio::test]
    async fn update_pr_body_patches_the_single_field() {
        let forge = forge(vec![response(201, PULL_REQUEST_CREATE)]);
        forge.update_pr_body(7, "new body").await.unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::PATCH,
                route: "/repos/stakk/probe/pulls/7",
                body: Some(serde_json::json!({ "body": "new body" })),
            }],
        );
    }

    #[tokio::test]
    async fn list_comments_reads_the_issue_comments_route() {
        let forge = forge(vec![response(200, ISSUE_COMMENTS)]);
        let comments = forge.list_comments(1).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, 6);
        assert_eq!(comments[0].body, "hello2");
        assert_requests(
            &forge,
            &[Expected {
                method: Method::GET,
                route: "/repos/stakk/probe/issues/1/comments?page=1&limit=50",
                body: None,
            }],
        );
    }

    #[tokio::test]
    async fn list_comments_walks_a_full_page_then_a_short_one() {
        let forge = forge(vec![
            response(200, &page_of_comments(PAGE_LIMIT, 100)),
            response(200, ISSUE_COMMENTS),
        ]);
        let comments = forge.list_comments(1).await.unwrap();
        assert_eq!(comments.len(), PAGE_LIMIT + 1);
        assert_eq!(comments[0].id, 100);
        assert_eq!(comments[PAGE_LIMIT].id, 6);
        assert_requests(
            &forge,
            &[
                Expected {
                    method: Method::GET,
                    route: "/repos/stakk/probe/issues/1/comments?page=1&limit=50",
                    body: None,
                },
                Expected {
                    method: Method::GET,
                    route: "/repos/stakk/probe/issues/1/comments?page=2&limit=50",
                    body: None,
                },
            ],
        );
    }

    #[tokio::test]
    async fn create_comment_posts_the_body() {
        let single: serde_json::Value = serde_json::from_str(ISSUE_COMMENTS).unwrap();
        let forge = forge(vec![response(201, &single[0].to_string())]);
        let comment = forge.create_comment(1, "hello2").await.unwrap();
        assert_eq!(comment.id, 6);
        assert_eq!(comment.body, "hello2");
        assert_requests(
            &forge,
            &[Expected {
                method: Method::POST,
                route: "/repos/stakk/probe/issues/1/comments",
                body: Some(serde_json::json!({ "body": "hello2" })),
            }],
        );
    }

    #[tokio::test]
    async fn update_comment_patches_the_body() {
        let single: serde_json::Value = serde_json::from_str(ISSUE_COMMENTS).unwrap();
        let forge = forge(vec![response(200, &single[0].to_string())]);
        forge.update_comment(6, "edited").await.unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::PATCH,
                route: "/repos/stakk/probe/issues/comments/6",
                body: Some(serde_json::json!({ "body": "edited" })),
            }],
        );
    }

    #[tokio::test]
    async fn delete_comment_sends_delete_and_accepts_an_empty_204() {
        let forge = forge(vec![response(204, "")]);
        forge.delete_comment(6).await.unwrap();
        assert_requests(
            &forge,
            &[Expected {
                method: Method::DELETE,
                route: "/repos/stakk/probe/issues/comments/6",
                body: None,
            }],
        );
    }

    #[test]
    fn a_trailing_slash_on_the_api_base_is_trimmed() {
        let forge = ForgejoForge::with_transport(
            MockTransport::new(vec![]),
            TOKEN,
            "stakk".into(),
            "probe".into(),
            "http://localhost:3999/api/v1/",
        );
        let request = forge
            .build_request(Method::GET, "/repos/stakk/probe/pulls", None)
            .unwrap();
        assert_eq!(
            request.uri().to_string(),
            "http://localhost:3999/api/v1/repos/stakk/probe/pulls"
        );
    }

    #[tokio::test]
    async fn unauthorized_and_forbidden_map_to_auth_failed() {
        for status in [401, 403] {
            let forge = forge(vec![response(
                status,
                r#"{"message":"token is malformed: token contains an invalid number of segments","url":"http://localhost:3999/api/swagger"}"#,
            )]);
            let err = forge.find_pr_for_branch("feat-a").await.unwrap_err();
            match err {
                ForgeError::AuthFailed { message, .. } => {
                    assert!(message.starts_with("token is malformed"), "{message}");
                }
                other => panic!("expected AuthFailed for {status}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn not_found_maps_to_api_with_forgejos_message() {
        let forge = forge(vec![response(404, ERROR_NOT_FOUND)]);
        let err = forge.list_comments(1).await.unwrap_err();
        match err {
            ForgeError::Api { message, .. } => {
                assert!(
                    message.contains("The target couldn't be found."),
                    "{message}"
                );
                assert!(message.contains("404"), "{message}");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_json_error_body_is_carried_raw() {
        let forge = forge(vec![response(500, "upstream exploded")]);
        let err = forge.update_pr_base(7, "main").await.unwrap_err();
        match err {
            ForgeError::Api { message, .. } => {
                assert!(message.contains("upstream exploded"), "{message}");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_error_body_falls_back_to_the_status() {
        assert_eq!(
            error_message(StatusCode::BAD_GATEWAY, "  "),
            "HTTP 502 Bad Gateway"
        );
    }

    #[tokio::test]
    async fn transport_failures_map_to_api_with_the_failure_as_source() {
        let forge = ForgejoForge::with_transport(
            FailingTransport,
            TOKEN,
            "stakk".into(),
            "probe".into(),
            API_BASE,
        );
        let err = forge.find_pr_for_branch("feat-a").await.unwrap_err();
        match err {
            ForgeError::Api { message, source } => {
                assert!(message.contains("connection refused"), "{message}");
                assert!(source.to_string().contains("connection refused"));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stack_methods_answer_unavailable_without_a_request() {
        let forge = forge(vec![]);
        assert!(matches!(
            forge.get_stacks_for_pr(1).await,
            Err(ForgeError::StacksUnavailable { .. })
        ));
        assert!(matches!(
            forge.create_stack(&[1, 2]).await,
            Err(ForgeError::StacksUnavailable { .. })
        ));
        assert!(matches!(
            forge.add_to_stack(1, &[3]).await,
            Err(ForgeError::StacksUnavailable { .. })
        ));
        assert!(matches!(
            forge.unstack(1).await,
            Err(ForgeError::StacksUnavailable { .. })
        ));
        assert!(forge.transport.requests.lock().unwrap().is_empty());

        let Err(ForgeError::StacksUnavailable { message, .. }) = forge.get_stacks_for_pr(1).await
        else {
            unreachable!()
        };
        assert_eq!(message, "Forgejo has no native stacked pull requests");
    }
}
