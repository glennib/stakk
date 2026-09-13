//! Forge trait and implementations.
//!
//! All forge interaction (GitHub, Forgejo) goes through the `Forge` trait.
//! The core submission logic never imports forge-specific types directly.

pub mod comment;
pub mod detect;
pub mod forgejo;
pub mod github;

use clap::ValueEnum as _;
use miette::Diagnostic;
use thiserror::Error;

/// Which forge a remote is on and which client talks to it.
///
/// An explicit `--forge` (`STAKK_FORGE`, `forge` in `stakk.toml`) names it
/// outright; otherwise the remote's host decides through
/// [`detect::classify`]: the public hosts are built in, and any other host
/// is looked up in the host table (`--host HOST=FORGE`, `STAKK_HOSTS`,
/// `hosts` in `stakk.toml`, `GH_HOST`). There is no default kind: a
/// self-hosted Forgejo and a GitHub Enterprise Server look alike from a URL,
/// and guessing wrong would send a token to the wrong API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    /// GitHub, including GitHub Enterprise Server.
    Github,
    /// Forgejo, including codeberg.org, and Gitea, which speaks the same API.
    Forgejo,
}

impl ForgeKind {
    /// The forge's name as it appears in prose: `GitHub` or `Forgejo`.
    ///
    /// [`std::fmt::Display`] prints the clap value name (`github`, `forgejo`)
    /// instead: it is what config defaults are re-injected as, what a
    /// [`detect::HostRule`] prints, and what the post-probe hint quotes
    /// inside a `hosts` entry.
    pub fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Forgejo => "Forgejo",
        }
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pv = self
            .to_possible_value()
            .expect("all variants have possible values");
        f.write_str(pv.get_name())
    }
}

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
        help(
            "the token for this host was rejected — expired, revoked, or missing a scope. \
             Re-check it; see `stakk docs auth`"
        )
    )]
    AuthFailed {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A stacks collection endpoint answered 404: the forge does not offer
    /// native stacked pull requests here (on GitHub, a host or repository
    /// without the feature — GitHub Enterprise Server, for example).
    ///
    /// Deliberately no `help` on this variant or on
    /// [`ForgeError::StackConflict`]: every stack call is wrapped by
    /// `submit::wrap_stack_err` into a `SubmitError`, and miette renders
    /// only the outermost diagnostic's help — `#[source]` chains the error
    /// *messages*, not the diagnostics. The user-facing guidance lives on
    /// `SubmitError::StacksUnavailable` / `StackReconcileFailed`, the one
    /// copy that actually renders; a second copy here would be dead prose
    /// that can only drift from it.
    #[error("native stacked pull requests are not available on this repository: {message}")]
    #[diagnostic(code(stakk::forge::stacks_unavailable))]
    StacksUnavailable {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A stack mutation conflicted with current server state (409/422 —
    /// e.g. PRs that do not chain, or PRs already held by another stack —
    /// or a 404 on a numbered stack route: the addressed stack no longer
    /// exists). The reconciliation converges on it by rebuilding; when it
    /// surfaces to the user it does so wrapped in
    /// `SubmitError::StackReconcileFailed`, whose help carries the
    /// re-run advice (see [`ForgeError::StacksUnavailable`] on why no
    /// `help` sits here).
    #[error("stack operation conflicted with current server state: {message}")]
    #[diagnostic(code(stakk::forge::stack_conflict))]
    StackConflict {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// A pull request, forge-agnostic.
#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub html_url: String,
    pub title: String,
    pub base_ref: String,
    /// The PR body/description text.
    pub body: Option<String>,
}

/// A server-side stack of pull requests, forge-agnostic.
///
/// Only *open* stacks reach this type — implementations skip stacks the
/// forge reports as closed (fully merged away).
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

    /// List the *open* server-side stacks containing the given PR (empty if
    /// none). A 404 from the forge maps to [`ForgeError::StacksUnavailable`]:
    /// the feature is not offered on this repository.
    fn get_stacks_for_pr(
        &self,
        pr_number: u64,
    ) -> impl std::future::Future<Output = Result<Vec<ForgeStack>, ForgeError>> + Send;

    /// Create a server-side stack from PR numbers ordered bottom-to-top.
    /// A 404 maps to [`ForgeError::StacksUnavailable`], like
    /// [`Forge::get_stacks_for_pr`].
    fn create_stack(
        &self,
        pr_numbers: &[u64],
    ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send;

    /// Append PRs (ordered bottom-to-top) on top of an existing stack.
    /// A stack that no longer exists surfaces as
    /// [`ForgeError::StackConflict`] — a state race for the caller to
    /// converge on, never [`ForgeError::StacksUnavailable`].
    fn add_to_stack(
        &self,
        stack_number: u64,
        pr_numbers: &[u64],
    ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send;

    /// Remove all unmerged PRs from a stack, dissolving it if it empties.
    /// Unstacking a stack that no longer exists succeeds — absence is the
    /// goal state.
    fn unstack(
        &self,
        stack_number: u64,
    ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_kind_displays_as_the_clap_value_name() {
        assert_eq!(ForgeKind::Github.to_string(), "github");
        assert_eq!(ForgeKind::Forgejo.to_string(), "forgejo");
    }

    #[test]
    fn forge_kind_label_is_the_prose_name() {
        assert_eq!(ForgeKind::Github.label(), "GitHub");
        assert_eq!(ForgeKind::Forgejo.label(), "Forgejo");
    }

    /// The config file spells the kind the way the flag does.
    #[test]
    fn forge_kind_deserializes_from_the_lowercase_value_name() {
        #[derive(serde::Deserialize)]
        struct Holder {
            forge: ForgeKind,
        }
        let holder: Holder = toml::from_str(r#"forge = "forgejo""#).unwrap();
        assert_eq!(holder.forge, ForgeKind::Forgejo);
        let holder: Holder = toml::from_str(r#"forge = "github""#).unwrap();
        assert_eq!(holder.forge, ForgeKind::Github);
        assert!(toml::from_str::<Holder>(r#"forge = "GitHub""#).is_err());
    }

    #[test]
    fn auth_failed_help_names_no_forge_specific_tool() {
        let err = ForgeError::AuthFailed {
            message: "401".into(),
            source: "unauthorized".into(),
        };
        let help = miette::Diagnostic::help(&err).unwrap().to_string();
        assert!(help.contains("stakk docs auth"), "{help}");
        assert!(!help.contains("gh "), "{help}");
    }
}
