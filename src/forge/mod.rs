//! Forge trait and implementations.
//!
//! All forge interaction (GitHub, etc.) goes through the `Forge` trait. The
//! core submission logic never imports forge-specific types directly.

pub mod comment;
pub mod github;

use miette::Diagnostic;
use thiserror::Error;

/// Errors from forge operations.
#[derive(Debug, Error, Diagnostic)]
pub enum ForgeError {
    #[error("API error: {message}")]
    #[diagnostic(code(stakk::forge::api))]
    Api {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("authentication failed: {message}")]
    #[diagnostic(
        code(stakk::forge::auth_failed),
        help("your token may have expired — run `gh auth login` to re-authenticate")
    )]
    AuthFailed {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("native stacked pull requests are not available on this repository: {message}")]
    #[diagnostic(
        code(stakk::forge::stacks_unavailable),
        help(
            "GitHub's stacked pull requests are a preview feature that must be enabled for the \
             repository — set `--native-stacks auto` to use it only where available, or `off`"
        )
    )]
    StacksUnavailable {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("stack operation conflicted with current server state: {message}")]
    #[diagnostic(code(stakk::forge::stack_conflict))]
    StackConflict {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// State of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

/// A pull request, forge-agnostic.
#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub html_url: String,
    pub title: String,
    #[expect(
        dead_code,
        reason = "part of PR data model, not yet consumed by submission logic"
    )]
    pub head_ref: String,
    pub base_ref: String,
    #[expect(
        dead_code,
        reason = "part of PR data model, not yet consumed by submission logic"
    )]
    pub state: PrState,
    /// The PR body/description text.
    pub body: Option<String>,
}

/// A server-side stack of pull requests, forge-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeStack {
    /// Stack number, used to address the stack in later calls.
    pub number: u64,
    /// Numbers of the stack's *open* PRs, bottom-to-top. Merged and closed
    /// members are filtered out by the implementation.
    pub open_pr_numbers: Vec<u64>,
}

/// A comment on a pull request.
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: u64,
    pub body: String,
}

/// Parameters for creating a pull request.
#[derive(Debug, Clone)]
pub struct CreatePrParams {
    pub title: String,
    pub head: String,
    pub base: String,
    pub body: Option<String>,
    pub draft: bool,
}

/// Trait for interacting with a code forge (GitHub, Forgejo, etc.).
///
/// All methods return forge-agnostic types. Implementations handle the
/// translation to/from forge-specific APIs.
pub trait Forge: Send + Sync {
    /// Get the username of the authenticated user.
    fn get_authenticated_user(
        &self,
    ) -> impl std::future::Future<Output = Result<String, ForgeError>> + Send;

    /// Find an open PR with the given head branch.
    fn find_pr_for_branch(
        &self,
        head: &str,
    ) -> impl std::future::Future<Output = Result<Option<PullRequest>, ForgeError>> + Send;

    /// Create a new pull request.
    fn create_pr(
        &self,
        params: CreatePrParams,
    ) -> impl std::future::Future<Output = Result<PullRequest, ForgeError>> + Send;

    /// Update the base branch of an existing PR.
    fn update_pr_base(
        &self,
        pr_number: u64,
        new_base: &str,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// Update the title of an existing PR.
    fn update_pr_title(
        &self,
        pr_number: u64,
        title: &str,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// List all comments on a PR.
    fn list_comments(
        &self,
        pr_number: u64,
    ) -> impl std::future::Future<Output = Result<Vec<Comment>, ForgeError>> + Send;

    /// Create a comment on a PR.
    fn create_comment(
        &self,
        pr_number: u64,
        body: &str,
    ) -> impl std::future::Future<Output = Result<Comment, ForgeError>> + Send;

    /// Update an existing comment.
    fn update_comment(
        &self,
        comment_id: u64,
        body: &str,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// Update the body/description of a pull request.
    fn update_pr_body(
        &self,
        pr_number: u64,
        body: &str,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// Delete a comment by ID.
    fn delete_comment(
        &self,
        comment_id: u64,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// List the server-side stacks containing the given PR (empty if none).
    fn get_stacks_for_pr(
        &self,
        pr_number: u64,
    ) -> impl std::future::Future<Output = Result<Vec<ForgeStack>, ForgeError>> + Send;

    /// Create a server-side stack from PR numbers ordered bottom-to-top.
    fn create_stack(
        &self,
        pr_numbers: &[u64],
    ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send;

    /// Append PRs (ordered bottom-to-top) on top of an existing stack.
    fn add_to_stack(
        &self,
        stack_number: u64,
        pr_numbers: &[u64],
    ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send;

    /// Remove all unmerged PRs from a stack, dissolving it if it empties.
    fn unstack(
        &self,
        stack_number: u64,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// Probe whether the forge offers native server-side stacks on this
    /// repository. `Ok(true)`/`Ok(false)` are definitive answers; `Err`
    /// means the answer could not be determined (e.g. a transient network
    /// failure) and callers should avoid destructive decisions based on it.
    fn supports_native_stacks(
        &self,
    ) -> impl std::future::Future<Output = Result<bool, ForgeError>> + Send;
}
