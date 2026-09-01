//! Hand-rolled transport for GitHub's stacked-pull-requests endpoints.
//!
//! **This module exists to be deleted.** Once octocrab ships typed support
//! for the stacks API (<https://github.com/XAMPPRocky/octocrab/issues/934>),
//! replace the calls in `super` with the typed equivalents and remove this
//! file — nothing outside `forge::github` touches it.
//!
//! Why the requests are built by hand instead of going through octocrab's
//! `get`/`post` helpers: the stacks endpoints require
//! `X-GitHub-Api-Version: 2026-03-10`, and octocrab stamps its own baked-in
//! `2022-11-28` onto every request its `build_request` produces (from the
//! `[package.metadata.github-api]` table in octocrab's own Cargo.toml).
//! `OctocrabBuilder::add_header` *appends* rather than replaces, so both
//! versions go on the wire as `"2022-11-28,2026-03-10"` and GitHub rejects
//! the request with a 400. Building the `http::Request` ourselves and
//! sending it through the public `Octocrab::execute` skips the stamping
//! while keeping octocrab's service layers, which still provide
//! authentication, the base URI (github.com or an Enterprise Server), and
//! the `User-Agent` header GitHub requires.
//!
//! Error handling is also deliberately not octocrab's: its
//! `map_github_error` re-parses error bodies into a struct whose `errors`
//! field must be an array, and GitHub's version-rejection body carries a
//! *string* there, degrading the real status code to a serde error. Here
//! the status code is read first and the body is parsed leniently.

use http::Method;
use http_body_util::BodyExt;
use octocrab::Octocrab;
use serde::Deserialize;

use super::super::ForgeError;
use super::super::ForgeStack;

/// API version required by the stacked-pull-requests endpoints.
const STACKS_API_VERSION: &str = "2026-03-10";

/// What a 404 answer means on the route being called.
///
/// GitHub signals "stacked pull requests are not offered on this repository"
/// with a 404, but only on the collection routes (`/stacks`,
/// `/stacks?pull_request=`) is that the whole story. The numbered routes
/// (`/stacks/{n}/add`, `/stacks/{n}/unstack`) are only reached after a
/// collection call on the same repository succeeded, so a 404 there means
/// the addressed stack no longer exists — merged away or dissolved since the
/// lookup. That is a state race to converge on, not an availability answer.
#[derive(Debug, Clone, Copy)]
enum NotFound {
    /// The feature is not offered on this repository (definitive).
    FeatureUnavailable,
    /// The addressed stack no longer exists; the caller converges by
    /// rebuilding, like any other state conflict.
    StackGone,
    /// The addressed stack no longer exists *and* absence is the goal
    /// state — the 404 counts as success.
    StackAlreadyGone,
}

/// List the open server-side stacks containing the given PR.
pub async fn get_stacks_for_pr(
    client: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<ForgeStack>, ForgeError> {
    let route = format!("/repos/{owner}/{repo}/stacks?pull_request={pr_number}");
    let body = request(
        client,
        Method::GET,
        &route,
        None,
        NotFound::FeatureUnavailable,
    )
    .await?;
    let stacks: Vec<StackDto> = parse_response(&body)?;
    Ok(stacks
        .into_iter()
        .filter(|s| s.open)
        .map(convert_stack)
        .collect())
}

/// Create a server-side stack from PR numbers ordered bottom-to-top.
pub async fn create_stack(
    client: &Octocrab,
    owner: &str,
    repo: &str,
    pr_numbers: &[u64],
) -> Result<ForgeStack, ForgeError> {
    let route = format!("/repos/{owner}/{repo}/stacks");
    let payload = serde_json::json!({ "pull_requests": pr_numbers });
    let body = request(
        client,
        Method::POST,
        &route,
        Some(&payload),
        NotFound::FeatureUnavailable,
    )
    .await?;
    let stack: StackDto = parse_response(&body)?;
    Ok(convert_stack(stack))
}

/// Append PRs (ordered bottom-to-top) on top of an existing stack.
///
/// A stack that no longer exists surfaces as [`ForgeError::StackConflict`],
/// which the reconciliation converges on by rebuilding — not as
/// [`ForgeError::StacksUnavailable`], since this route is only reached after
/// a collection call proved the feature exists here.
pub async fn add_to_stack(
    client: &Octocrab,
    owner: &str,
    repo: &str,
    stack_number: u64,
    pr_numbers: &[u64],
) -> Result<ForgeStack, ForgeError> {
    let route = format!("/repos/{owner}/{repo}/stacks/{stack_number}/add");
    let payload = serde_json::json!({ "pull_requests": pr_numbers });
    let body = request(
        client,
        Method::POST,
        &route,
        Some(&payload),
        NotFound::StackGone,
    )
    .await?;
    let stack: StackDto = parse_response(&body)?;
    Ok(convert_stack(stack))
}

/// Remove all unmerged PRs from a stack. The server answers 200 (stack
/// remains, with merged members) or 204 (stack dissolved, empty body);
/// either way the response body is not needed.
///
/// A 404 — the stack no longer exists — counts as success: absence is
/// exactly the state this call is after, and the route is only reached
/// after a collection call proved the feature exists here.
pub async fn unstack(
    client: &Octocrab,
    owner: &str,
    repo: &str,
    stack_number: u64,
) -> Result<(), ForgeError> {
    let route = format!("/repos/{owner}/{repo}/stacks/{stack_number}/unstack");
    request(
        client,
        Method::POST,
        &route,
        None,
        NotFound::StackAlreadyGone,
    )
    .await?;
    Ok(())
}

/// Build a stacks request by hand and send it through `Octocrab::execute`.
///
/// The route stays relative: octocrab's `BaseUriLayer` absolutizes it
/// against the configured base URI, and its `AuthHeaderLayer` attaches the
/// token to requests whose URI carries no authority — which is exactly the
/// relative-route case, the same path octocrab's own typed helpers take.
///
/// Returns the raw response body on 2xx, a mapped [`ForgeError`] otherwise.
async fn request(
    client: &Octocrab,
    method: Method,
    route: &str,
    payload: Option<&serde_json::Value>,
    not_found: NotFound,
) -> Result<Vec<u8>, ForgeError> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(route)
        .header("x-github-api-version", STACKS_API_VERSION)
        .header(http::header::ACCEPT, "application/vnd.github+json");

    let body = if let Some(value) = payload {
        builder = builder.header(http::header::CONTENT_TYPE, "application/json");
        serde_json::to_string(value).map_err(|e| ForgeError::Api {
            message: format!("failed to serialize stack request body: {e}"),
            source: Box::new(e),
        })?
    } else {
        builder = builder.header(http::header::CONTENT_LENGTH, "0");
        String::new()
    };

    let request = builder.body(body).map_err(|e| ForgeError::Api {
        message: format!("failed to build stack request: {e}"),
        source: Box::new(e),
    })?;

    let response = client.execute(request).await.map_err(|e| ForgeError::Api {
        message: format!("stack request failed: {e}"),
        source: Box::new(e),
    })?;

    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| ForgeError::Api {
            message: format!("failed to read stack response body: {e}"),
            source: Box::new(e),
        })?
        .to_bytes()
        .to_vec();

    status_outcome(status, &body, not_found)?;
    Ok(body)
}

/// Decide what a response status means for the route: `Ok` when the call
/// achieved its goal (2xx, or a 404 whose route-meaning is "already
/// absent"), the mapped [`ForgeError`] otherwise.
fn status_outcome(
    status: http::StatusCode,
    body: &[u8],
    not_found: NotFound,
) -> Result<(), ForgeError> {
    if status.is_success() {
        return Ok(());
    }
    if status == http::StatusCode::NOT_FOUND && matches!(not_found, NotFound::StackAlreadyGone) {
        return Ok(());
    }
    Err(map_status(status, body, not_found))
}

/// Map a non-2xx stacks response to a [`ForgeError`], status code first.
///
/// A 404 is mapped per route — see [`NotFound`]. GitHub's own `gh stack`
/// client reads every 404 as "feature not offered here", which is sound on
/// the collection routes but conflates a deleted stack with unavailability
/// on the numbered ones. 409/422 signal that a stack mutation conflicts
/// with current server state (PRs that do not chain, or PRs already held
/// by another stack).
fn map_status(status: http::StatusCode, body: &[u8], not_found: NotFound) -> ForgeError {
    let message = error_message(body, status);
    let source: Box<dyn std::error::Error + Send + Sync> =
        format!("GitHub answered {status}: {message}").into();
    match status {
        http::StatusCode::NOT_FOUND => match not_found {
            NotFound::FeatureUnavailable => ForgeError::StacksUnavailable { message, source },
            // `StackAlreadyGone` is consumed as a success in
            // `status_outcome`; a stray one degrades to the conflict
            // reading, which a re-run converges on.
            NotFound::StackGone | NotFound::StackAlreadyGone => {
                ForgeError::StackConflict { message, source }
            }
        },
        http::StatusCode::CONFLICT | http::StatusCode::UNPROCESSABLE_ENTITY => {
            ForgeError::StackConflict { message, source }
        }
        http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN => {
            ForgeError::AuthFailed { message, source }
        }
        _ => ForgeError::Api { message, source },
    }
}

/// Pull the `message` field out of a GitHub error body, leniently.
///
/// GitHub error bodies are JSON objects with a `message` string, but the
/// other fields vary by endpoint (the version-rejection body carries a
/// *string*-valued `errors`, for example), so nothing here insists on a
/// shape beyond the one field it reads. Falls back to the raw body text,
/// then to the status code alone.
fn error_message(body: &[u8], status: http::StatusCode) -> String {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body)
        && let Some(message) = value.get("message").and_then(|m| m.as_str())
    {
        return message.to_string();
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        format!("HTTP {status}")
    } else {
        text.to_string()
    }
}

fn parse_response<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, ForgeError> {
    serde_json::from_slice(body).map_err(|e| ForgeError::Api {
        message: format!("failed to parse stack response: {e}"),
        source: Box::new(e),
    })
}

/// Wire format of a stack object from the stacks endpoints
/// (API version 2026-03-10). Fields the reconciliation does not consume
/// (`id`, `node_id`, `url`, `base`, `created_at`) are ignored by serde.
#[derive(Debug, Deserialize)]
struct StackDto {
    number: u64,
    /// `false` once the stack has fully merged away. Closed stacks still
    /// appear in list responses; they are filtered out before conversion.
    open: bool,
    pull_requests: Vec<StackPrDto>,
}

/// Wire format of a stack member PR.
#[derive(Debug, Deserialize)]
struct StackPrDto {
    number: u64,
    state: String,
    merged_at: Option<String>,
}

fn convert_stack(dto: StackDto) -> ForgeStack {
    ForgeStack {
        number: dto.number,
        open_pr_numbers: dto
            .pull_requests
            .into_iter()
            .filter(|p| p.state == "open" && p.merged_at.is_none())
            .map(|p| p.number)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live wire shape of a stack, captured from
    /// `GET /repos/glennib/stakk/stacks` (2026-09-01) with irrelevant
    /// values shortened.
    const STACK_JSON: &str = r#"{
        "id": 341775,
        "number": 204,
        "node_id": "PRS_kwDORUKw6s4ABTcP",
        "url": "https://api.github.com/repos/glennib/stakk/stacks/204",
        "base": {"ref": "main"},
        "open": true,
        "created_at": "2026-08-13T13:44:58Z",
        "pull_requests": [
            {"number": 201, "state": "open", "draft": false,
             "merged_at": null,
             "head": {"ref": "feat-a", "sha": "aaaa"}},
            {"number": 203, "state": "closed", "draft": false,
             "merged_at": "2026-08-13T13:45:53Z",
             "head": {"ref": "feat-b", "sha": "bbbb"}},
            {"number": 205, "state": "closed", "draft": false,
             "merged_at": null,
             "head": {"ref": "feat-c", "sha": "cccc"}}
        ]
    }"#;

    #[test]
    fn stack_parses_and_keeps_only_open_unmerged_prs() {
        let dto: StackDto = serde_json::from_str(STACK_JSON).unwrap();
        assert!(dto.open);
        let stack = convert_stack(dto);
        // #203 is merged, #205 is closed without merging — only #201 stays.
        assert_eq!(
            stack,
            ForgeStack {
                number: 204,
                open_pr_numbers: vec![201],
            }
        );
    }

    #[test]
    fn closed_stacks_are_filtered_from_list_responses() {
        // A fully merged stack keeps appearing in list responses with
        // `"open": false` — observed live; the listing must drop it so the
        // reconciliation never tries to unstack a stack that is already
        // history.
        let closed = STACK_JSON.replace(r#""open": true"#, r#""open": false"#);
        let listing = format!("[{closed}]");
        let stacks: Vec<StackDto> = serde_json::from_str(&listing).unwrap();
        let open: Vec<ForgeStack> = stacks
            .into_iter()
            .filter(|s| s.open)
            .map(convert_stack)
            .collect();
        assert!(open.is_empty());
    }

    #[test]
    fn missing_open_field_fails_loudly() {
        // Like `LogEntryRaw`, the DTO is deliberately not serde-defaulted:
        // an API-shape change should fail as a parse error rather than
        // silently treating every stack as closed or open.
        let without_open = STACK_JSON.replace(r#""open": true,"#, "");
        assert!(serde_json::from_str::<StackDto>(&without_open).is_err());
    }

    #[test]
    fn status_mapping_is_read_before_the_body_shape() {
        // GitHub's version-rejection body carries a *string*-valued
        // `errors`; octocrab's own error mapping chokes on it. Ours only
        // reads `message`.
        let body = br#"{"message": "The version you specified is not a supported version.", "errors": "bad", "documentation_url": "https://docs.github.com"}"#;
        let err = map_status(
            http::StatusCode::BAD_REQUEST,
            body,
            NotFound::FeatureUnavailable,
        );
        match err {
            ForgeError::Api { message, .. } => {
                assert!(message.starts_with("The version you specified"));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn not_found_on_collection_routes_maps_to_stacks_unavailable() {
        let body = br#"{"message": "Not Found"}"#;
        assert!(matches!(
            map_status(
                http::StatusCode::NOT_FOUND,
                body,
                NotFound::FeatureUnavailable
            ),
            ForgeError::StacksUnavailable { .. }
        ));
    }

    /// On the numbered routes a 404 means the addressed stack vanished — a
    /// state race the reconcile converges on, never an availability answer
    /// that would misadvise switching `--native-stacks` away from the
    /// feature.
    #[test]
    fn not_found_on_numbered_routes_is_a_conflict_not_unavailability() {
        let body = br#"{"message": "Not Found"}"#;
        assert!(matches!(
            map_status(http::StatusCode::NOT_FOUND, body, NotFound::StackGone),
            ForgeError::StackConflict { .. }
        ));
    }

    /// For `unstack`, a vanished stack *is* the goal state: the 404 counts
    /// as success, while every other failure on the route still fails.
    #[test]
    fn unstack_of_an_already_gone_stack_counts_as_success() {
        let body = br#"{"message": "Not Found"}"#;
        assert!(
            status_outcome(
                http::StatusCode::NOT_FOUND,
                body,
                NotFound::StackAlreadyGone
            )
            .is_ok()
        );
        assert!(
            status_outcome(http::StatusCode::CONFLICT, body, NotFound::StackAlreadyGone).is_err()
        );
        // The success reading is scoped to the already-gone routes.
        assert!(status_outcome(http::StatusCode::NOT_FOUND, body, NotFound::StackGone).is_err());
    }

    #[test]
    fn conflict_statuses_map_to_stack_conflict() {
        for status in [
            http::StatusCode::CONFLICT,
            http::StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(matches!(
                map_status(
                    status,
                    br#"{"message": "conflict"}"#,
                    NotFound::FeatureUnavailable
                ),
                ForgeError::StackConflict { .. }
            ));
        }
    }

    #[test]
    fn error_message_falls_back_to_body_then_status() {
        assert_eq!(
            error_message(b"plain text error", http::StatusCode::BAD_GATEWAY),
            "plain text error"
        );
        assert_eq!(
            error_message(b"", http::StatusCode::BAD_GATEWAY),
            "HTTP 502 Bad Gateway"
        );
    }
}
