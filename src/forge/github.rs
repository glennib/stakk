//! GitHub implementation of the Forge trait using octocrab.

use octocrab::Octocrab;
use octocrab::models::CommentId;
use octocrab::models::IssueState;

use super::Comment;
use super::CreatePrParams;
use super::Forge;
use super::ForgeError;
use super::ForgeStack;
use super::PrState;
use super::PullRequest;

/// API version required by the stacked-pull-requests endpoints (public
/// preview). octocrab sends no `X-GitHub-Api-Version` header by default,
/// which GitHub treats as 2022-11-28 — too old for the stack routes.
const STACKS_API_VERSION: &str = "2026-03-10";

/// GitHub implementation of the `Forge` trait.
pub struct GitHubForge {
    client: Octocrab,
    /// Separate client that carries the `x-github-api-version` header the
    /// stack endpoints require, so the main client's requests keep GitHub's
    /// default API version.
    stacks_client: Octocrab,
    owner: String,
    repo: String,
}

impl GitHubForge {
    /// Create a new `GitHubForge` for the given repository.
    pub fn new(token: &str, owner: String, repo: String) -> Result<Self, ForgeError> {
        let wrap_build_error = |e: octocrab::Error| {
            let message = format!("failed to create GitHub client: {e}");
            ForgeError::Api {
                message,
                source: Box::new(e),
            }
        };
        let client = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(wrap_build_error)?;
        let stacks_client = Octocrab::builder()
            .personal_token(token.to_string())
            .add_header(
                http::header::HeaderName::from_static("x-github-api-version"),
                STACKS_API_VERSION.to_string(),
            )
            .build()
            .map_err(wrap_build_error)?;

        Ok(Self {
            client,
            stacks_client,
            owner,
            repo,
        })
    }

    fn stacks_route(&self) -> String {
        format!("/repos/{}/{}/stacks", self.owner, self.repo)
    }
}

impl Forge for GitHubForge {
    async fn get_authenticated_user(&self) -> Result<String, ForgeError> {
        let user = self
            .client
            .current()
            .user()
            .await
            .map_err(map_octocrab_error)?;
        Ok(user.login)
    }

    async fn find_pr_for_branch(&self, head: &str) -> Result<Option<PullRequest>, ForgeError> {
        let qualified_head = format!("{}:{head}", self.owner);
        let pulls = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .head(qualified_head)
            .state(octocrab::params::State::Open)
            .send()
            .await
            .map_err(map_octocrab_error)?;

        Ok(pulls.items.into_iter().next().map(convert_pr))
    }

    async fn create_pr(&self, params: CreatePrParams) -> Result<PullRequest, ForgeError> {
        let pulls = self.client.pulls(&self.owner, &self.repo);
        let mut builder = pulls.create(&params.title, &params.head, &params.base);

        if let Some(body) = &params.body {
            builder = builder.body(body);
        }

        if params.draft {
            builder = builder.draft(true);
        }

        let pr = builder.send().await.map_err(map_octocrab_error)?;

        Ok(convert_pr(pr))
    }

    async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<(), ForgeError> {
        self.client
            .pulls(&self.owner, &self.repo)
            .update(pr_number)
            .base(new_base)
            .send()
            .await
            .map_err(map_octocrab_error)?;
        Ok(())
    }

    async fn update_pr_title(&self, pr_number: u64, title: &str) -> Result<(), ForgeError> {
        self.client
            .pulls(&self.owner, &self.repo)
            .update(pr_number)
            .title(title)
            .send()
            .await
            .map_err(map_octocrab_error)?;
        Ok(())
    }

    async fn list_comments(&self, pr_number: u64) -> Result<Vec<Comment>, ForgeError> {
        let comments = self
            .client
            .issues(&self.owner, &self.repo)
            .list_comments(pr_number)
            .send()
            .await
            .map_err(map_octocrab_error)?;

        Ok(comments
            .items
            .into_iter()
            .map(|c| Comment {
                id: c.id.into_inner(),
                body: c.body.unwrap_or_default(),
            })
            .collect())
    }

    async fn create_comment(&self, pr_number: u64, body: &str) -> Result<Comment, ForgeError> {
        let comment = self
            .client
            .issues(&self.owner, &self.repo)
            .create_comment(pr_number, body)
            .await
            .map_err(map_octocrab_error)?;

        Ok(Comment {
            id: comment.id.into_inner(),
            body: comment.body.unwrap_or_default(),
        })
    }

    async fn update_comment(&self, comment_id: u64, body: &str) -> Result<(), ForgeError> {
        self.client
            .issues(&self.owner, &self.repo)
            .update_comment(CommentId::from(comment_id), body)
            .await
            .map_err(map_octocrab_error)?;
        Ok(())
    }

    async fn update_pr_body(&self, pr_number: u64, body: &str) -> Result<(), ForgeError> {
        self.client
            .pulls(&self.owner, &self.repo)
            .update(pr_number)
            .body(body)
            .send()
            .await
            .map_err(map_octocrab_error)?;
        Ok(())
    }

    async fn delete_comment(&self, comment_id: u64) -> Result<(), ForgeError> {
        self.client
            .issues(&self.owner, &self.repo)
            .delete_comment(CommentId::from(comment_id))
            .await
            .map_err(map_octocrab_error)?;
        Ok(())
    }

    async fn get_stacks_for_pr(&self, pr_number: u64) -> Result<Vec<ForgeStack>, ForgeError> {
        let stacks: Vec<StackDto> = self
            .stacks_client
            .get(self.stacks_route(), Some(&[("pull_request", pr_number)]))
            .await
            .map_err(map_octocrab_stack_error)?;
        Ok(stacks.into_iter().map(convert_stack).collect())
    }

    async fn create_stack(&self, pr_numbers: &[u64]) -> Result<ForgeStack, ForgeError> {
        let stack: StackDto = self
            .stacks_client
            .post(
                self.stacks_route(),
                Some(&serde_json::json!({ "pull_requests": pr_numbers })),
            )
            .await
            .map_err(map_octocrab_stack_error)?;
        Ok(convert_stack(stack))
    }

    async fn add_to_stack(
        &self,
        stack_number: u64,
        pr_numbers: &[u64],
    ) -> Result<ForgeStack, ForgeError> {
        let stack: StackDto = self
            .stacks_client
            .post(
                format!("{}/{stack_number}/add", self.stacks_route()),
                Some(&serde_json::json!({ "pull_requests": pr_numbers })),
            )
            .await
            .map_err(map_octocrab_stack_error)?;
        Ok(convert_stack(stack))
    }

    async fn supports_native_stacks(&self) -> Result<bool, ForgeError> {
        let result: Result<Vec<serde_json::Value>, octocrab::Error> = self
            .stacks_client
            .get(self.stacks_route(), Some(&[("per_page", 1_u64)]))
            .await;
        match result.map_err(map_octocrab_stack_error) {
            Ok(_) => Ok(true),
            // A definitive 404: the preview feature is not enabled here.
            Err(ForgeError::StacksUnavailable { .. }) => Ok(false),
            // Anything else (auth, network, rate limit) is not an answer.
            Err(e) => Err(e),
        }
    }

    async fn unstack(&self, stack_number: u64) -> Result<(), ForgeError> {
        // The response is 200 (stack remains) or 204 (stack dissolved, empty
        // body); the typed `post` can't deserialize an empty body, so use the
        // raw `_post` and reinstate octocrab's GitHub-error mapping manually
        // to keep status codes flowing into `map_octocrab_stack_error`.
        let response = self
            .stacks_client
            ._post(
                format!("{}/{stack_number}/unstack", self.stacks_route()),
                None::<&()>,
            )
            .await
            .map_err(map_octocrab_stack_error)?;
        octocrab::map_github_error(response)
            .await
            .map_err(map_octocrab_stack_error)?;
        Ok(())
    }
}

/// Convert an octocrab pull request into the forge-agnostic type.
fn convert_pr(pr: octocrab::models::pulls::PullRequest) -> PullRequest {
    PullRequest {
        number: pr.number,
        html_url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
        title: pr.title.unwrap_or_default(),
        head_ref: pr.head.ref_field,
        base_ref: pr.base.ref_field,
        state: map_pr_state(pr.state.as_ref(), pr.merged_at.is_some()),
        body: pr.body,
    }
}

fn map_octocrab_error(e: octocrab::Error) -> ForgeError {
    let is_auth_error = matches!(
        &e,
        octocrab::Error::GitHub { source, .. }
            if source.status_code == http::StatusCode::UNAUTHORIZED
                || source.status_code == http::StatusCode::FORBIDDEN
    );
    if is_auth_error {
        let message = match &e {
            octocrab::Error::GitHub { source, .. } => source.message.clone(),
            _ => unreachable!(),
        };
        return ForgeError::AuthFailed {
            message,
            source: Box::new(e),
        };
    }
    let message = e.to_string();
    ForgeError::Api {
        message,
        source: Box::new(e),
    }
}

fn map_pr_state(state: Option<&IssueState>, has_merged_at: bool) -> PrState {
    if has_merged_at {
        PrState::Merged
    } else if state == Some(&IssueState::Closed) {
        PrState::Closed
    } else {
        PrState::Open
    }
}

/// Wire format of a stack object from the stacks endpoints.
#[derive(Debug, serde::Deserialize)]
struct StackDto {
    number: u64,
    pull_requests: Vec<StackPrDto>,
}

/// Wire format of a stack member PR.
#[derive(Debug, serde::Deserialize)]
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

/// Error mapping for the stack endpoints, layered on `map_octocrab_error`.
///
/// A 404 on a stack route means the stacked-PRs preview feature is not
/// enabled for the repository — we only reach these routes after having
/// talked to the repo successfully. 409/422 signal that a stack mutation
/// conflicts with current server state (e.g. PRs that don't chain).
fn map_octocrab_stack_error(e: octocrab::Error) -> ForgeError {
    if let octocrab::Error::GitHub { source, .. } = &e {
        if source.status_code == http::StatusCode::NOT_FOUND {
            return ForgeError::StacksUnavailable {
                message: source.message.clone(),
                source: Box::new(e),
            };
        }
        if source.status_code == http::StatusCode::CONFLICT
            || source.status_code == http::StatusCode::UNPROCESSABLE_ENTITY
        {
            return ForgeError::StackConflict {
                message: source.message.clone(),
                source: Box::new(e),
            };
        }
    }
    map_octocrab_error(e)
}
