//! GitHub implementation of the Forge trait using octocrab.

use octocrab::Octocrab;
use octocrab::models::CommentId;

use super::Comment;
use super::CreatePrParams;
use super::Forge;
use super::ForgeError;
use super::PullRequest;

/// GitHub implementation of the `Forge` trait.
pub struct GitHubForge {
    client: Octocrab,
    owner: String,
    repo: String,
}

impl GitHubForge {
    /// Create a new `GitHubForge` for the given repository.
    ///
    /// `api_base_uri` is `None` for github.com, where octocrab's default
    /// (`https://api.github.com`) applies, and `Some` for a GitHub Enterprise
    /// Server host. It is a plain string rather than a parsed remote so this
    /// module stays independent of `jj::remote`.
    pub fn new(
        token: &str,
        owner: String,
        repo: String,
        api_base_uri: Option<&str>,
    ) -> Result<Self, ForgeError> {
        let mut builder = Octocrab::builder().personal_token(token.to_string());
        if let Some(uri) = api_base_uri {
            builder = builder.base_uri(uri).map_err(|e| {
                let message = format!("invalid GitHub API base URI '{uri}': {e}");
                ForgeError::Api {
                    message,
                    source: Box::new(e),
                }
            })?;
        }

        let client = builder.build().map_err(|e| {
            let message = format!("failed to create GitHub client: {e}");
            ForgeError::Api {
                message,
                source: Box::new(e),
            }
        })?;

        Ok(Self {
            client,
            owner,
            repo,
        })
    }
}

impl Forge for GitHubForge {
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
}

/// Convert an octocrab pull request into the forge-agnostic type.
fn convert_pr(pr: octocrab::models::pulls::PullRequest) -> PullRequest {
    PullRequest {
        number: pr.number,
        html_url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
        title: pr.title.unwrap_or_default(),
        base_ref: pr.base.ref_field,
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
