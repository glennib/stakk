//! GitHub implementation of the Forge trait using octocrab.

use octocrab::Octocrab;
use octocrab::models::CommentId;
use octocrab::models::IssueState;

use super::Comment;
use super::CreatePrParams;
use super::Forge;
use super::ForgeError;
use super::PrState;
use super::PullRequest;

/// GitHub implementation of the `Forge` trait.
pub struct GitHubForge {
    client: Octocrab,
    owner: String,
    repo: String,
}

impl GitHubForge {
    /// Create a new `GitHubForge` for the given repository.
    pub fn new(token: &str, owner: String, repo: String) -> Result<Self, ForgeError> {
        let client = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(|e| ForgeError::Api {
                message: format!("failed to create GitHub client: {e}"),
            })?;

        Ok(Self {
            client,
            owner,
            repo,
        })
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

        Ok(pulls.items.into_iter().next().map(|pr| PullRequest {
            number: pr.number,
            html_url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
            title: pr.title.unwrap_or_default(),
            head_ref: pr.head.ref_field,
            base_ref: pr.base.ref_field,
            state: map_pr_state(pr.state, pr.merged_at.is_some()),
        }))
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

        Ok(PullRequest {
            number: pr.number,
            html_url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
            title: pr.title.unwrap_or_default(),
            head_ref: pr.head.ref_field,
            base_ref: pr.base.ref_field,
            state: PrState::Open,
        })
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

    async fn get_repo_default_branch(&self) -> Result<String, ForgeError> {
        let repo = self
            .client
            .repos(&self.owner, &self.repo)
            .get()
            .await
            .map_err(map_octocrab_error)?;

        repo.default_branch.ok_or_else(|| ForgeError::Api {
            message: "repository has no default branch".to_string(),
        })
    }
}

fn map_octocrab_error(e: octocrab::Error) -> ForgeError {
    if let octocrab::Error::GitHub { source, .. } = &e
        && (source.status_code == http::StatusCode::UNAUTHORIZED
            || source.status_code == http::StatusCode::FORBIDDEN)
    {
        return ForgeError::AuthFailed {
            message: source.message.clone(),
        };
    }
    ForgeError::Api {
        message: e.to_string(),
    }
}

fn map_pr_state(state: Option<IssueState>, has_merged_at: bool) -> PrState {
    if has_merged_at {
        PrState::Merged
    } else if state == Some(IssueState::Closed) {
        PrState::Closed
    } else {
        PrState::Open
    }
}
