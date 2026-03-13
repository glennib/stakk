use clap::Args;

use crate::cli::graph::GraphArgs;

/// Arguments for the `submit` subcommand.
#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The bookmark to submit as a pull request. If omitted, shows an
    /// interactive selection.
    pub bookmark: Option<String>,

    /// Show what would be done without actually doing it.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub graph: GraphArgs,

    /// Create pull requests as drafts.
    #[arg(long, env = "STAKK_DRAFT")]
    pub draft: bool,

    /// Git remote to push to.
    #[arg(long, default_value = "origin", env = "STAKK_REMOTE")]
    pub remote: String,

    /// Temporarily disable auto-merge on PRs while updating their base
    /// branches, then re-enable it. Prevents GitHub from auto-merging PRs
    /// during stack reordering. Experimental.
    #[arg(long, env = "STAKK_EXPERIMENTAL_SUSPEND_AUTO_MERGE")]
    pub experimental_suspend_auto_merge: bool,

    /// Path to a custom minijinja template for stack comments.
    ///
    /// The template is rendered with minijinja and receives the following
    /// context:
    ///
    ///   stack             — list of entries (see below)
    ///   stack_size        — total number of entries
    ///   default_branch    — name of the trunk branch (e.g. "main")
    ///   current_bookmark  — the bookmark being submitted
    ///   stakk_url         — URL to the stakk project
    ///
    /// Each entry in stack has:
    ///
    ///   bookmark_name  — bookmark name
    ///   pr_url         — full URL to the pull request
    ///   pr_number      — PR number
    ///   title          — PR title
    ///   base           — base branch name
    ///   is_draft       — whether the PR is a draft
    ///   position       — 1-based position in the stack
    ///   is_current     — true for the PR being submitted
    ///
    /// Example template:
    ///
    ///  Stack ({{ stack_size }} PRs, merges into `{{ default_branch }}`):
    ///   {% for entry in stack %}
    ///   - {{ entry.pr_url }}{% if entry.is_current %} 👈{% endif %}
    ///   {%- endfor %}
    #[expect(
        clippy::doc_lazy_continuation,
        reason = "endfor must align with the for-loop, not the list item"
    )]
    #[arg(long, env = "STAKK_TEMPLATE", verbatim_doc_comment)]
    pub template: Option<String>,
}
