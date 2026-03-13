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

    #[expect(dead_code, reason = "used in later milestones for PR lookup errors")]
    #[error("PR not found: #{number}")]
    #[diagnostic(code(stakk::forge::pr_not_found))]
    PrNotFound { number: u64 },

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

    #[expect(dead_code, reason = "used in later milestones for rate limit handling")]
    #[error("rate limited; retry after {retry_after_seconds}s")]
    #[diagnostic(
        code(stakk::forge::rate_limited),
        help("wait {retry_after_seconds}s and retry")
    )]
    RateLimited { retry_after_seconds: u64 },

    #[error("failed to toggle auto-merge on PR #{pr_number}: {message}")]
    #[diagnostic(
        code(stakk::forge::auto_merge_toggle_failed),
        help("you may need to re-enable auto-merge manually on the PR")
    )]
    AutoMergeToggleFailed { pr_number: u64, message: String },
}

/// The merge method used for a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// Auto-merge state of a pull request.
#[derive(Debug, Clone)]
pub struct AutoMergeState {
    pub merge_method: MergeMethod,
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
    #[expect(dead_code, reason = "populated by forge, read in future milestones")]
    pub title: String,
    #[expect(dead_code, reason = "populated by forge, read in future milestones")]
    pub head_ref: String,
    pub base_ref: String,
    #[expect(dead_code, reason = "populated by forge, read in future milestones")]
    pub state: PrState,
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

    /// Get the auto-merge state of a PR, if enabled.
    fn get_auto_merge_state(
        &self,
        pr_number: u64,
    ) -> impl std::future::Future<Output = Result<Option<AutoMergeState>, ForgeError>> + Send;

    /// Disable auto-merge on a PR.
    fn suspend_auto_merge(
        &self,
        pr_number: u64,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;

    /// Re-enable auto-merge on a PR with the given merge method.
    fn restore_auto_merge(
        &self,
        pr_number: u64,
        method: MergeMethod,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;
}
