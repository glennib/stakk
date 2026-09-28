//! GitHub implementation of the Forge trait using octocrab.

mod stacks;

use octocrab::Octocrab;
use octocrab::models::CommentId;

use super::Comment;
use super::CreatePrParams;
use super::Forge;
use super::ForgeError;
use super::ForgeStack;
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
            .map_err(map_base_update_error)?;
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

    // The four stack methods go through `stacks`, a hand-rolled transport
    // for the preview endpoints — see that module for why octocrab's typed
    // helpers cannot serve them yet.

    async fn get_stacks_for_pr(&self, pr_number: u64) -> Result<Vec<ForgeStack>, ForgeError> {
        stacks::get_stacks_for_pr(&self.client, &self.owner, &self.repo, pr_number).await
    }

    async fn create_stack(&self, pr_numbers: &[u64]) -> Result<ForgeStack, ForgeError> {
        stacks::create_stack(&self.client, &self.owner, &self.repo, pr_numbers).await
    }

    async fn add_to_stack(
        &self,
        stack_number: u64,
        pr_numbers: &[u64],
    ) -> Result<ForgeStack, ForgeError> {
        stacks::add_to_stack(
            &self.client,
            &self.owner,
            &self.repo,
            stack_number,
            pr_numbers,
        )
        .await
    }

    async fn unstack(&self, stack_number: u64) -> Result<(), ForgeError> {
        stacks::unstack(&self.client, &self.owner, &self.repo, stack_number).await
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

/// Map a failed base update, telling apart the one refusal whose cause
/// stakk can name: GitHub will not change the base of a PR that is a member
/// of a native stack, and answers 422 with the reason in `errors`.
fn map_base_update_error(e: octocrab::Error) -> ForgeError {
    if let octocrab::Error::GitHub { source, .. } = &e
        && source.status_code == http::StatusCode::UNPROCESSABLE_ENTITY
        && let Some(message) = stack_membership_refusal(source.errors.as_deref())
    {
        return ForgeError::BaseLockedByStack {
            message,
            source: Box::new(e),
        };
    }
    map_octocrab_error(e)
}

/// The message of the validation error saying the PR is part of a stack,
/// if `errors` carries one.
fn stack_membership_refusal(errors: Option<&[serde_json::Value]>) -> Option<String> {
    errors?
        .iter()
        .filter_map(|error| error.get("message")?.as_str())
        .find(|message| message.contains("part of a stack"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stack_membership_refusal_finds_the_stack_message() {
        // The body GitHub sent for a base change on a stacked PR (#299).
        let errors = vec![json!({
            "code": "invalid",
            "field": "base",
            "message": "Cannot change the base branch because the pull request is part of a stack.",
            "resource": "PullRequest",
        })];
        assert_eq!(
            stack_membership_refusal(Some(&errors)).as_deref(),
            Some("Cannot change the base branch because the pull request is part of a stack.")
        );
    }

    #[test]
    fn stack_membership_refusal_ignores_other_validation_errors() {
        let errors = vec![
            json!({"code": "invalid", "field": "base", "resource": "PullRequest"}),
            json!({"code": "custom", "message": "There are no new commits between base and head."}),
            json!("a bare string"),
        ];
        assert_eq!(stack_membership_refusal(Some(&errors)), None);
        assert_eq!(stack_membership_refusal(None), None);
    }
}
