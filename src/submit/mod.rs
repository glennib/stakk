//! Three-phase submission: analyze, plan, execute.
//!
//! Takes a change graph and forge implementation and submits bookmarks as
//! stacked pull requests, updating existing PRs idempotently.

mod trailers;

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

use crate::cli::submit::NativeStacks;
use crate::cli::submit::PrMode;
use crate::cli::submit::SyncPrContent;
use crate::cli::submit::TrailerHandling;
use crate::forge::CreatePrParams;
use crate::forge::Forge;
use crate::forge::ForgeError;
use crate::forge::ForgeStack;
use crate::forge::PullRequest;
use crate::forge::comment::STAKK_REPO_URL;
use crate::forge::comment::StackCommentContext;
use crate::forge::comment::StackCommentData;
use crate::forge::comment::StackEntry;
use crate::forge::comment::StackEntryContext;
use crate::forge::comment::StackPlacement;
use crate::forge::comment::find_stack_comment;
use crate::forge::comment::find_stack_in_body;
use crate::forge::comment::format_stack_comment;
use crate::forge::comment::splice_stack_into_body;
use crate::forge::comment::strip_stack_from_body;
use crate::forge::comment::with_comment_preamble;
use crate::graph::types::BookmarkSegment;
use crate::graph::types::SegmentCommit;
use crate::jj::Jj;
use crate::jj::JjError;
use crate::jj::runner::JjRunner;
use crate::markdown::unwrap::unwrap_markdown;
use crate::submit::trailers::split_trailers;

/// Errors from the submission pipeline.
#[derive(Debug, Error, Diagnostic)]
pub enum SubmitError {
    /// A segment in the change graph has no bookmark name.
    #[error("segment for change {change_id} has no bookmark name")]
    #[diagnostic(
        code(stakk::submit::segment_missing_bookmark),
        help("this is likely a bug in stakk — please report it")
    )]
    SegmentMissingBookmark { change_id: String },

    /// Failed to look up an existing PR for a bookmark.
    #[error("failed to check for existing PR for '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::pr_lookup_failed),
        help("check your network connection and GitHub token permissions")
    )]
    PrLookupFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// A selected assignment targets a commit that is not on the selected
    /// path.
    #[error(
        "bookmark assignment '{bookmark}' targets change {change_id}, which is not on the \
         selected path"
    )]
    #[diagnostic(
        code(stakk::submit::assignment_off_path),
        help("this indicates a bug in the selection layer — please report it")
    )]
    AssignmentOffPath { bookmark: String, change_id: String },

    /// A change ID matches more than one commit on the selected path.
    #[error(
        "change {change_id} is divergent: it matches more than one commit on the selected path"
    )]
    #[diagnostic(
        code(stakk::submit::divergent_change),
        help(
            "resolve the divergence first, e.g. `jj abandon` the copy you do not want, then re-run"
        )
    )]
    DivergentChange { change_id: String },

    /// Failed to list local bookmarks before creating new ones.
    #[error("failed to list local bookmarks")]
    #[diagnostic(
        code(stakk::submit::bookmark_list_failed),
        help("check that `jj bookmark list` runs in this repo")
    )]
    BookmarkListFailed {
        #[source]
        source: JjError,
    },

    /// One or more planned bookmark names are already taken.
    #[error("bookmark(s) already exist: {}", bookmarks.join(", "))]
    #[diagnostic(
        code(stakk::submit::bookmark_names_taken),
        help(
            "the names were free when the selection was made — another bookmark has appeared \
             since; rename with --new REV=NAME, or reuse the existing bookmark with --keep NAME"
        )
    )]
    BookmarkNamesTaken { bookmarks: Vec<String> },

    /// Failed to create a local bookmark during execution.
    #[error("failed to create bookmark '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::bookmark_create_failed),
        help(
            "check that the name is not already taken (`jj bookmark list`) and that the target \
             commit still exists"
        )
    )]
    BookmarkCreateFailed {
        bookmark: String,
        #[source]
        source: JjError,
    },

    /// Failed to push a bookmark to the remote.
    #[error("failed to push bookmark '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::push_failed),
        help("ensure the bookmark exists and the remote is reachable")
    )]
    PushFailed {
        bookmark: String,
        #[source]
        source: JjError,
    },

    /// Failed to update the base branch of an existing PR.
    #[error("failed to update PR base for '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::base_update_failed),
        help(
            "the PR exists but its base branch could not be changed — check your token permissions"
        )
    )]
    BaseUpdateFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to create a new PR.
    #[error("failed to create PR for '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::pr_create_failed),
        help("check your token permissions and that the head branch exists on the remote")
    )]
    PrCreateFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to create or update a stack comment on a PR.
    #[error("failed to manage stack comment on PR #{pr_number}")]
    #[diagnostic(
        code(stakk::submit::comment_failed),
        help("check your token permissions for commenting on PRs")
    )]
    CommentFailed {
        pr_number: u64,
        #[source]
        source: ForgeError,
    },

    /// Failed to render a stack comment template.
    #[error("template rendering failed: {message}")]
    #[diagnostic(
        code(stakk::submit::template_render_failed),
        help("check the template syntax (minijinja/Jinja2)")
    )]
    TemplateRenderFailed { message: String },

    /// Failed to update a PR body.
    #[error("failed to update body of PR #{pr_number}")]
    #[diagnostic(
        code(stakk::submit::body_update_failed),
        help("check your token permissions for updating PR descriptions")
    )]
    BodyUpdateFailed {
        pr_number: u64,
        #[source]
        source: ForgeError,
    },

    /// Failed to sync the title of an existing PR.
    #[error("failed to sync title of PR #{pr_number} for '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::title_sync_failed),
        help("the PR exists but its title could not be updated — check your token permissions")
    )]
    TitleSyncFailed {
        pr_number: u64,
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to sync the body of an existing PR.
    #[error("failed to sync body of PR #{pr_number} for '{bookmark}'")]
    #[diagnostic(
        code(stakk::submit::body_sync_failed),
        help("the PR exists but its body could not be updated — check your token permissions")
    )]
    BodySyncFailed {
        pr_number: u64,
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Native stacks were requested but the forge does not offer them here.
    #[error("native stacked pull requests are not available on this repository")]
    #[diagnostic(
        code(stakk::submit::stacks_unavailable),
        help(
            "your branches were pushed and PRs were created/updated normally — only the \
             server-side stack linkage was skipped. GitHub's stacked pull requests are not \
             available everywhere (GitHub Enterprise Server does not have them); set \
             `--native-stacks auto` to use the feature only where available, or `ignore` (env: \
             STAKK_NATIVE_STACKS)"
        )
    )]
    StacksUnavailable {
        #[source]
        source: ForgeError,
    },

    /// Failed to reconcile the server-side stack after PRs were submitted.
    #[error("failed to reconcile the server-side stack")]
    #[diagnostic(
        code(stakk::submit::stack_reconcile_failed),
        help(
            "your branches were pushed and PRs were created/updated normally — only the \
             server-side stack linkage failed. Re-running `stakk submit` retries the \
             reconciliation from scratch"
        )
    )]
    StackReconcileFailed {
        #[source]
        source: ForgeError,
    },
}

/// Wrap a stack-API error, routing the not-available case to its dedicated
/// variant so miette renders the switch-mode guidance.
fn wrap_stack_err(source: ForgeError) -> SubmitError {
    match source {
        ForgeError::StacksUnavailable { .. } => SubmitError::StacksUnavailable { source },
        _ => SubmitError::StackReconcileFailed { source },
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Phase 1 output: the segments relevant to a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionAnalysis {
    /// Segments from trunk to the target bookmark, inclusive.
    /// Ordered trunk-to-leaf (same as `BranchStack::segments`).
    pub segments: Vec<BookmarkSegment>,
    /// The default branch name (e.g., "main").
    pub default_branch: String,
}

/// One bookmark's planned actions.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these are independent action flags, not a state machine"
)]
pub struct BookmarkPlan {
    /// The bookmark name (first from `segment.bookmark_names`).
    pub bookmark_name: String,
    /// The base branch for this PR (default branch or previous bookmark).
    pub base: String,
    /// PR title (derived from first commit description).
    pub title: String,
    /// PR body built from commit descriptions, if any.
    pub body: Option<String>,
    /// Existing PR if one was found on GitHub.
    pub existing_pr: Option<PullRequest>,
    /// Whether the bookmark needs pushing.
    pub needs_push: bool,
    /// Whether a new PR must be created.
    pub needs_create: bool,
    /// Whether the existing PR's base needs updating.
    pub needs_base_update: bool,
    /// Whether the existing PR's title should be synced from commits.
    pub needs_title_sync: bool,
    /// Whether the existing PR's body should be synced from commits.
    pub needs_body_sync: bool,
}

/// A bookmark assignment for a commit in the submission stack.
///
/// Produced by the selection layer (the TUI, or future non-interactive
/// selection sources) and consumed by `analysis_from_selection`; defined
/// here so the submission engine does not depend on UI types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkAssignment {
    /// The jj change ID for this commit.
    pub change_id: String,
    /// Shortest unique change ID prefix, for display.
    pub short_change_id: String,
    /// The bookmark name (existing or newly generated).
    pub bookmark_name: String,
    /// `true` if stakk must run `jj bookmark create` for this bookmark.
    pub is_new: bool,
}

/// A local bookmark the execute phase must create before pushing.
///
/// Creation is deferred to execution so that `--dry-run` never mutates the
/// repository; the plan lists pending creations instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkCreation {
    /// The bookmark name to create.
    pub bookmark_name: String,
    /// The jj change the bookmark points at.
    pub change_id: String,
    /// Shortest unique change ID prefix, for display.
    pub short_change_id: String,
}

/// Phase 2 output: the full submission plan.
#[derive(Debug)]
pub struct SubmissionPlan {
    /// Local bookmarks to create before pushing. Derived from the
    /// selection's `is_new` assignments; empty when every selected bookmark
    /// already exists.
    pub bookmark_creations: Vec<BookmarkCreation>,
    /// Per-bookmark plans, ordered trunk-to-leaf.
    pub bookmark_plans: Vec<BookmarkPlan>,
    /// The remote name to push to.
    pub remote: String,
    /// Whether to create PRs as regular or draft.
    pub pr_mode: PrMode,
    /// The default branch name (e.g., "main").
    pub default_branch: String,
}

/// Phase 3 output: what was actually done.
#[derive(Debug)]
pub struct SubmissionResult {
    /// Stack entries for all submitted bookmarks.
    pub stack_entries: Vec<StackEntry>,
}

// ---------------------------------------------------------------------------
// Phase 1: Analysis
// ---------------------------------------------------------------------------

/// Build a submission analysis from an explicit selection.
///
/// This is the only phase-1 constructor: every submission — interactive or
/// flag-driven — arrives here.
///
/// `path` is the full trunk-to-tip commit chain of the selected stack and
/// `assignments` (trunk-to-leaf) name the commits that become segment
/// boundaries. Commits between boundaries belong to the boundary above them.
/// Commits above the last boundary are not part of the submission.
///
/// No bookmark lookup happens: boundaries are matched by change ID, so
/// bookmarks that do not exist yet (`is_new` assignments) work without
/// creating them first or rebuilding the graph. That keeps `--dry-run` free
/// of side effects; the execute phase performs the actual `jj bookmark
/// create` calls.
///
/// Contract: every assignment's `change_id` must be present on `path`
/// (guaranteed by the TUI, whose rows come from the selected path). An
/// assignment whose change ID is not on the path errors with
/// `AssignmentOffPath`; a change ID matching more than one path commit (a
/// divergent change) errors with `DivergentChange` — silently dropping or
/// duplicating a boundary would otherwise desync the analysis from the
/// bookmark creations scheduled for execution.
///
/// Where a segment carries several bookmark names, the resulting segment
/// keeps only the assigned name — the name the user actually chose. With
/// several consecutive folded segments the folded commits are ordered
/// strictly newest-first, per `BookmarkSegment::commits`' convention.
pub fn analysis_from_selection(
    path: &[SegmentCommit],
    assignments: &[BookmarkAssignment],
    default_branch: &str,
) -> Result<SubmissionAnalysis, SubmitError> {
    let boundaries: HashMap<&str, &BookmarkAssignment> = assignments
        .iter()
        .map(|a| (a.change_id.as_str(), a))
        .collect();

    let mut segments = Vec::new();
    let mut consumed: HashSet<&str> = HashSet::new();
    // Oldest-first buffer of commits belonging to the next boundary above.
    let mut pending: Vec<SegmentCommit> = Vec::new();

    for commit in path {
        pending.push(commit.clone());
        if let Some(assignment) = boundaries.get(commit.change_id.as_str()) {
            if !consumed.insert(assignment.change_id.as_str()) {
                return Err(SubmitError::DivergentChange {
                    change_id: assignment.change_id.clone(),
                });
            }
            // Newest-first within the segment (internal convention).
            pending.reverse();
            segments.push(BookmarkSegment {
                bookmark_names: vec![assignment.bookmark_name.clone()],
                change_id: assignment.change_id.clone(),
                commits: std::mem::take(&mut pending),
            });
        }
    }

    if let Some(missed) = assignments
        .iter()
        .find(|a| !consumed.contains(a.change_id.as_str()))
    {
        return Err(SubmitError::AssignmentOffPath {
            bookmark: missed.bookmark_name.clone(),
            change_id: missed.change_id.clone(),
        });
    }

    Ok(SubmissionAnalysis {
        segments,
        default_branch: default_branch.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a PR body from segment commit descriptions.
///
/// - Single commit: lines after the first (the title line) become the body.
/// - Multiple commits: concatenate all descriptions with `---` separators.
/// - If the result is empty or whitespace-only, returns `None`.
///
/// Trailer blocks (Signed-off-by, Co-authored-by, Refs, etc.) are
/// removed from the unwrap pass and either dropped (`Strip`) or
/// reattached verbatim after unwrapping (`Keep`), so multi-line
/// trailer blocks survive intact.
fn build_pr_body(commits: &[SegmentCommit], trailers: TrailerHandling) -> Option<String> {
    if commits.is_empty() {
        return None;
    }

    let parts: Vec<String> = commits
        .iter()
        .enumerate()
        .filter_map(|(idx, c)| {
            let (body, trailer_block) = split_trailers(c.description.trim());
            // For the single-commit case, drop the title (first) line.
            let body_text = if commits.len() == 1 && idx == 0 {
                body.lines().skip(1).collect::<Vec<_>>().join("\n")
            } else {
                body.to_string()
            };
            let unwrapped = unwrap_markdown(body_text.trim());
            let kept_trailers = match trailers {
                TrailerHandling::Keep => trailer_block,
                TrailerHandling::Strip => None,
            };
            match (unwrapped.is_empty(), kept_trailers) {
                (true, None) => None,
                (true, Some(tb)) => Some(tb.to_string()),
                (false, None) => Some(unwrapped),
                (false, Some(tb)) => Some(format!("{unwrapped}\n\n{tb}")),
            }
        })
        .collect();

    if parts.is_empty() {
        return None;
    }

    let body = parts.join("\n\n---\n\n");
    if body.is_empty() { None } else { Some(body) }
}

// ---------------------------------------------------------------------------
// Phase 2: Planning
// ---------------------------------------------------------------------------

/// Query the forge to determine what actions are needed for each bookmark.
///
/// For each segment in the analysis, checks the forge for existing PRs and
/// determines whether to push, create, or update. `bookmark_creations` are
/// the local bookmarks the execute phase must create first (empty when every
/// selected bookmark already exists); taking them here keeps the returned
/// plan complete at construction.
pub async fn create_submission_plan<F: Forge>(
    analysis: &SubmissionAnalysis,
    bookmark_creations: Vec<BookmarkCreation>,
    forge: &F,
    remote: &str,
    pr_mode: PrMode,
    sync: SyncPrContent,
    trailers: TrailerHandling,
) -> Result<SubmissionPlan, SubmitError> {
    // Collect bookmark names for concurrent PR lookup.
    let bookmark_names: Vec<String> = analysis
        .segments
        .iter()
        .map(|seg| {
            seg.bookmark_names
                .first()
                .cloned()
                .ok_or_else(|| SubmitError::SegmentMissingBookmark {
                    change_id: seg.change_id.clone(),
                })
        })
        .collect::<Result<_, _>>()?;

    // Concurrently check for existing PRs for all bookmarks.
    let pr_futures: Vec<_> = bookmark_names
        .iter()
        .map(|name| forge.find_pr_for_branch(name))
        .collect();
    let pr_results = futures::future::join_all(pr_futures).await;

    let mut bookmark_plans = Vec::new();

    for (i, (segment, pr_result)) in analysis.segments.iter().zip(pr_results).enumerate() {
        let bookmark_name = bookmark_names[i].clone();

        let base = if i == 0 {
            analysis.default_branch.clone()
        } else {
            bookmark_names[i - 1].clone()
        };

        let title = segment.commits.first().map_or_else(
            || bookmark_name.clone(),
            |c| {
                c.description
                    .lines()
                    .next()
                    .unwrap_or(&c.description)
                    .to_string()
            },
        );

        let existing_pr = pr_result.map_err(|source| SubmitError::PrLookupFailed {
            bookmark: bookmark_name.clone(),
            source,
        })?;

        let needs_base_update = existing_pr.as_ref().is_some_and(|pr| pr.base_ref != base);

        let needs_create = existing_pr.is_none();

        let body = build_pr_body(&segment.commits, trailers);

        let wants_title = matches!(sync, SyncPrContent::Title | SyncPrContent::All);
        let wants_body = matches!(sync, SyncPrContent::Body | SyncPrContent::All);

        let needs_title_sync =
            wants_title && existing_pr.as_ref().is_some_and(|pr| pr.title != title);

        let needs_body_sync = wants_body
            && !needs_create
            && existing_pr.as_ref().is_some_and(|pr| {
                let existing_user_body = pr
                    .body
                    .as_deref()
                    .map(strip_stack_from_body)
                    .unwrap_or_default();
                let normalized_existing = unwrap_markdown(existing_user_body.trim());
                let normalized_new = body
                    .as_deref()
                    .map(|b| b.trim().to_string())
                    .unwrap_or_default();
                normalized_new != normalized_existing
            });

        bookmark_plans.push(BookmarkPlan {
            bookmark_name,
            base,
            title,
            body,
            existing_pr,
            needs_push: true,
            needs_create,
            needs_base_update,
            needs_title_sync,
            needs_body_sync,
        });
    }

    Ok(SubmissionPlan {
        bookmark_creations,
        bookmark_plans,
        remote: remote.to_string(),
        pr_mode,
        default_branch: analysis.default_branch.clone(),
    })
}

// ---------------------------------------------------------------------------
// Phase 2: Display (for --dry-run)
// ---------------------------------------------------------------------------

impl fmt::Display for SubmissionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let draft_label = if self.pr_mode == PrMode::Draft {
            ", draft"
        } else {
            ""
        };
        // No trailing newline — callers decide how to terminate the output.
        write!(
            f,
            "Submission plan ({} bookmark(s), remote: {}{draft_label}):",
            self.bookmark_plans.len(),
            self.remote,
        )?;

        for creation in &self.bookmark_creations {
            write!(
                f,
                "\n  Create bookmark {} at {}",
                creation.bookmark_name, creation.short_change_id,
            )?;
        }

        for bp in &self.bookmark_plans {
            write!(f, "\n  {} (base: {})", bp.bookmark_name, bp.base)?;
            if bp.needs_push {
                write!(f, "\n    - push bookmark to {}", self.remote)?;
            }
            if bp.needs_create {
                write!(f, "\n    - create PR: \"{}\"", bp.title)?;
            }
            if bp.needs_base_update
                && let Some(pr) = &bp.existing_pr
            {
                write!(
                    f,
                    "\n    - update PR #{} base: {} -> {}",
                    pr.number, pr.base_ref, bp.base,
                )?;
            }
            if bp.needs_title_sync
                && let Some(pr) = &bp.existing_pr
            {
                write!(f, "\n    - sync PR #{} title from commits", pr.number)?;
            }
            if bp.needs_body_sync
                && let Some(pr) = &bp.existing_pr
            {
                write!(f, "\n    - sync PR #{} body from commits", pr.number)?;
            }
            if !bp.needs_create
                && !bp.needs_base_update
                && !bp.needs_title_sync
                && !bp.needs_body_sync
                && let Some(pr) = &bp.existing_pr
            {
                write!(f, "\n    - PR #{} up to date", pr.number)?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Execution
// ---------------------------------------------------------------------------

/// Execute the submission plan: push, create PRs, update bases, manage
/// comments.
pub async fn execute_submission_plan<R: JjRunner, F: Forge>(
    plan: &SubmissionPlan,
    jj: &Jj<R>,
    forge: &F,
    comment_env: &minijinja::Environment<'_>,
    placement: StackPlacement,
    native: NativeStacks,
) -> Result<SubmissionResult, SubmitError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    // Check every pending name against the repo before creating any of them.
    // Selection already rejects taken names, but the repo can change in
    // between — and a mid-loop failure would leave some bookmarks created and
    // nothing pushed. Skipped when there is nothing to create, so plans
    // without new bookmarks cost no extra jj call.
    if !plan.bookmark_creations.is_empty() {
        pb.set_message("Checking bookmark names...");
        let reserved = jj
            .get_local_bookmark_names()
            .await
            .map_err(|source| SubmitError::BookmarkListFailed { source })?;
        let taken: Vec<String> = plan
            .bookmark_creations
            .iter()
            .map(|c| c.bookmark_name.clone())
            .filter(|name| reserved.contains(name))
            .collect();
        if !taken.is_empty() {
            return Err(SubmitError::BookmarkNamesTaken { bookmarks: taken });
        }
    }

    // Create pending local bookmarks first — pushes below require them to
    // exist. Creation is local-only, so the one-at-a-time interleaving rule
    // (which concerns pushes and base updates, #35) does not apply here.
    for creation in &plan.bookmark_creations {
        pb.set_message(format!("Creating bookmark: {}", creation.bookmark_name));
        jj.create_bookmark(&creation.bookmark_name, &creation.change_id)
            .await
            .map_err(|source| SubmitError::BookmarkCreateFailed {
                bookmark: creation.bookmark_name.clone(),
                source,
            })?;
    }

    let mut stack_entries = Vec::new();

    // Returns the body that is currently live on GitHub for this bookmark:
    // the commit-derived body if we just synced or created it, otherwise the
    // body fetched during planning.
    let effective_body = |bp: &BookmarkPlan| -> Option<String> {
        if bp.needs_create || bp.needs_body_sync {
            bp.body.clone()
        } else {
            bp.existing_pr.as_ref().and_then(|pr| pr.body.clone())
        }
    };

    // Process each bookmark trunk-to-leaf: push, update base, create PR.
    // Each bookmark must be fully processed before the next is pushed to
    // prevent transient empty diffs that trigger GitHub auto-close (#35).
    for bp in &plan.bookmark_plans {
        if bp.needs_push {
            pb.set_message(format!("Pushing bookmark: {}", bp.bookmark_name));
            jj.push_bookmark(&bp.bookmark_name, &plan.remote)
                .await
                .map_err(|source| SubmitError::PushFailed {
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
        }

        if bp.needs_base_update
            && let Some(pr) = &bp.existing_pr
        {
            pb.set_message(format!("Updating PR #{} base...", pr.number));
            forge
                .update_pr_base(pr.number, &bp.base)
                .await
                .map_err(|source| SubmitError::BaseUpdateFailed {
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
        }

        if bp.needs_title_sync
            && let Some(pr) = &bp.existing_pr
        {
            pb.set_message(format!("Syncing PR #{} title...", pr.number));
            forge
                .update_pr_title(pr.number, &bp.title)
                .await
                .map_err(|source| SubmitError::TitleSyncFailed {
                    pr_number: pr.number,
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
        }

        // Body syncs happen in one pass after the placement resolves, not
        // here: the interleaving rule (#35) covers pushes and base updates
        // only, and a body-resolved placement folds the sync into its own
        // body write below.

        let pr = if let Some(existing) = &bp.existing_pr {
            pb.println(format!(
                "  Existing PR #{}: {}",
                existing.number, existing.html_url,
            ));
            existing.clone()
        } else {
            pb.set_message(format!("Creating PR: {}", bp.title));
            let pr = forge
                .create_pr(CreatePrParams {
                    title: bp.title.clone(),
                    head: bp.bookmark_name.clone(),
                    base: bp.base.clone(),
                    body: bp.body.clone(),
                    draft: plan.pr_mode == PrMode::Draft,
                })
                .await
                .map_err(|source| SubmitError::PrCreateFailed {
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
            pb.println(format!("  Created PR #{}: {}", pr.number, pr.html_url));
            pr
        };

        stack_entries.push(StackEntry {
            bookmark_name: bp.bookmark_name.clone(),
            pr_url: pr.html_url.clone(),
            pr_number: pr.number,
        });
    }

    // Step 2b: When native stacks are requested, converge the forge's
    // server-side stack with the submitted PRs — *before* the text
    // placement, so the auto placements resolve on what actually happened
    // rather than on a prediction (there is no availability probe; the
    // reconcile's own outcome is the answer). A single PR is not a stack,
    // so single-bookmark submissions skip the registration entirely (a
    // stale server-side stack containing that PR is left alone — except
    // under `none`, whose retirement covers single-PR submissions too).
    let (native_state, native_err) = match native {
        // `ignore` never touches the stack API.
        NativeStacks::Ignore => (NativeState::Inactive, None),
        // `none` mirrors `--stack-placement none`: turning the feature off
        // retires the server-side stacks that are standing rather than
        // leaving them stale.
        NativeStacks::None => {
            pb.set_message("Retiring server-side stacks...");
            let submitted: Vec<u64> = stack_entries.iter().map(|e| e.pr_number).collect();
            retire_native_stacks(forge, &submitted, &pb).await;
            (NativeState::Inactive, None)
        }
        NativeStacks::On | NativeStacks::Auto if stack_entries.len() < 2 => {
            (NativeState::Inactive, None)
        }
        NativeStacks::On | NativeStacks::Auto => {
            pb.set_message("Reconciling server-side stack...");
            let desired: Vec<u64> = stack_entries.iter().map(|e| e.pr_number).collect();
            let outcome = reconcile_native_stack(forge, &desired, &pb).await;
            match (native, outcome) {
                (_, Ok(())) => (NativeState::Active, None),
                // Definitive: the feature is not offered on this repository.
                // `auto` promises to skip silently where that is the case.
                (NativeStacks::Auto, Err(SubmitError::StacksUnavailable { .. })) => {
                    (NativeState::Inactive, None)
                }
                // Not an answer (network trouble, rate limit): warn and let
                // the auto placements fall back to writing — a redundant
                // comment self-heals on the next successful run, while a
                // skipped update would leave *stale* stack info standing.
                (NativeStacks::Auto, Err(SubmitError::StackReconcileFailed { source })) => {
                    pb.println(format!(
                        "  Warning: could not reconcile the server-side stack: {source}. Stack \
                         comments and body fences are still updated this run; re-running `stakk \
                         submit` retries the reconciliation."
                    ));
                    (NativeState::Unknown, None)
                }
                // With `on`, any reconcile failure fails the submit — but
                // only after the body syncs and text placement below have
                // run, so that everything the error's help text promises
                // ("PRs were created/updated normally") has actually
                // happened. The placements resolve as on an unknown native
                // state: writing, never the destructive cleanup direction
                // (whether the state is definitively unavailable or merely
                // unknown makes no difference to a run that is going to
                // fail anyway).
                (_, Err(e)) => (NativeState::Unknown, Some(e)),
            }
        }
    };

    let effective = resolve_placement(placement, native_state);

    // Body syncs, in one pass for every placement. Skipped only when the
    // body-splice phase below covers them — a body-resolved placement on a
    // real stack folds each sync into its own body write, one API call
    // instead of two. This runs before step 3 so `effective_body` stays
    // truthful for every later reader.
    let body_splice_runs = effective == EffectivePlacement::Body && stack_entries.len() > 1;
    if !body_splice_runs {
        let sync_futures: Vec<_> = plan
            .bookmark_plans
            .iter()
            .filter_map(|bp| {
                if !bp.needs_body_sync {
                    return None;
                }
                let pr = bp.existing_pr.as_ref()?;
                let pr_number = pr.number;
                let bookmark = bp.bookmark_name.clone();
                let new_body = bp.body.clone().unwrap_or_default();
                Some(async move {
                    forge
                        .update_pr_body(pr_number, &new_body)
                        .await
                        .map_err(|source| SubmitError::BodySyncFailed {
                            pr_number,
                            bookmark,
                            source,
                        })
                })
            })
            .collect();
        if !sync_futures.is_empty() {
            pb.set_message("Syncing PR bodies...");
            for result in futures::future::join_all(sync_futures).await {
                result?;
            }
        }
    }

    // Step 3: Concurrently create/update stack comments on all PRs.
    //
    // Cleanup-resolved placements write no stack info and instead retire any
    // artifacts left on the PRs; single-bookmark submissions take the same
    // path because they are not a stack at all (the artifacts are stale
    // leftovers from when the PR belonged to a larger stack). Ignore-resolved
    // placements manage no artifacts at all: nothing is written, and existing
    // stack comments and body fences are left untouched.
    let cleanup_only = effective == EffectivePlacement::Cleanup || stack_entries.len() == 1;

    if effective == EffectivePlacement::Ignore {
    } else if cleanup_only {
        pb.set_message("Cleaning up stack artifacts...");
        let cleanup_futures: Vec<_> = stack_entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let bp = &plan.bookmark_plans[i];
                let existing_body = effective_body(bp).unwrap_or_default();
                // A PR created moments ago in this very run carries neither a
                // stack comment nor a body fence — nothing to look for.
                let created_now = bp.needs_create;
                let pb = &pb;
                async move {
                    if created_now {
                        return Ok(false);
                    }
                    cleanup_stack_artifacts(forge, entry.pr_number, &existing_body, pb).await
                }
            })
            .collect();
        let results = futures::future::join_all(cleanup_futures).await;
        let mut cleaned = 0usize;
        for result in results {
            if result? {
                cleaned += 1;
            }
        }
        if cleaned > 0 {
            pb.println(format!("  Removed stale stack info from {cleaned} PR(s)."));
        }
    } else if stack_entries.len() > 1 {
        pb.set_message("Updating stack comments...");
        let comment_data = StackCommentData {
            version: 0,
            stack: stack_entries.clone(),
        };

        let template = comment_env.get_template("stack_comment").map_err(|e| {
            SubmitError::TemplateRenderFailed {
                message: e.to_string(),
            }
        })?;

        // Build the shared entry contexts from stack_entries + bookmark_plans.
        let entry_contexts: Vec<StackEntryContext> = stack_entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let bp = &plan.bookmark_plans[i];
                StackEntryContext {
                    bookmark_name: entry.bookmark_name.clone(),
                    pr_url: entry.pr_url.clone(),
                    pr_number: entry.pr_number,
                    title: bp.title.clone(),
                    base: bp.base.clone(),
                    is_draft: plan.pr_mode == PrMode::Draft && bp.needs_create,
                    position: i + 1,
                    is_current: false, // set per-PR below
                    is_leaf: i + 1 == stack_entries.len(),
                }
            })
            .collect();

        match effective {
            EffectivePlacement::Comment => {
                let comment_futures: Vec<_> = stack_entries
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        let mut entries = entry_contexts.clone();
                        entries[i].is_current = true;
                        let ctx = StackCommentContext {
                            stack_size: entries.len(),
                            current_bookmark: entry.bookmark_name.clone(),
                            default_branch: plan.default_branch.clone(),
                            stakk_url: STAKK_REPO_URL.to_string(),
                            stack: entries,
                        };

                        let rendered = format_stack_comment(&comment_data, &ctx, &template)
                            .map(|s| with_comment_preamble(&s));
                        let pr_number = entry.pr_number;
                        let existing_body = effective_body(&plan.bookmark_plans[i]);
                        let pb = &pb;
                        async move {
                            let rendered = rendered?;
                            let existing_comments =
                                forge.list_comments(pr_number).await.map_err(|source| {
                                    SubmitError::CommentFailed { pr_number, source }
                                })?;

                            if let Some(existing) = find_stack_comment(&existing_comments) {
                                forge.update_comment(existing.id, &rendered).await.map_err(
                                    |source| SubmitError::CommentFailed { pr_number, source },
                                )?;
                            } else {
                                forge.create_comment(pr_number, &rendered).await.map_err(
                                    |source| SubmitError::CommentFailed { pr_number, source },
                                )?;

                                // Migration: if switching from body mode, strip
                                // the fenced section from the PR body.
                                if let Some(body) = &existing_body
                                    && find_stack_in_body(body).is_some()
                                {
                                    let stripped = strip_stack_from_body(body);
                                    if let Err(e) = forge.update_pr_body(pr_number, &stripped).await
                                    {
                                        pb.println(format!(
                                            "  Warning: failed to strip stack from PR \
                                             #{pr_number} body during migration: {e}"
                                        ));
                                    }
                                }
                            }
                            Ok::<(), SubmitError>(())
                        }
                    })
                    .collect();
                let comment_results = futures::future::join_all(comment_futures).await;
                for result in comment_results {
                    result?;
                }
                pb.println(format!(
                    "  Stack comment updated on {} PR(s).",
                    stack_entries.len()
                ));
            }
            EffectivePlacement::Body => {
                let body_futures: Vec<_> =
                    stack_entries
                        .iter()
                        .enumerate()
                        .map(|(i, entry)| {
                            let mut entries = entry_contexts.clone();
                            entries[i].is_current = true;
                            let ctx = StackCommentContext {
                                stack_size: entries.len(),
                                current_bookmark: entry.bookmark_name.clone(),
                                default_branch: plan.default_branch.clone(),
                                stakk_url: STAKK_REPO_URL.to_string(),
                                stack: entries,
                            };

                            let rendered = format_stack_comment(&comment_data, &ctx, &template);
                            let pr_number = entry.pr_number;
                            let bp = &plan.bookmark_plans[i];
                            let existing_body = effective_body(bp).unwrap_or_default();
                            let had_fence = find_stack_in_body(&existing_body).is_some();
                            let pb = &pb;
                            async move {
                                let rendered = rendered?;
                                let new_body = splice_stack_into_body(&existing_body, &rendered);
                                forge.update_pr_body(pr_number, &new_body).await.map_err(
                                    |source| SubmitError::BodyUpdateFailed { pr_number, source },
                                )?;

                                // Migration: if no existing fenced section was
                                // found,
                                // check for an old stack comment and delete it.
                                if !had_fence {
                                    let comments =
                                        forge.list_comments(pr_number).await.map_err(|source| {
                                            SubmitError::CommentFailed { pr_number, source }
                                        })?;
                                    if let Some(old) = find_stack_comment(&comments)
                                        && let Err(e) = forge.delete_comment(old.id).await
                                    {
                                        pb.println(format!(
                                            "  Warning: failed to delete old stack comment on PR \
                                             #{pr_number} during migration: {e}"
                                        ));
                                    }
                                }
                                Ok::<(), SubmitError>(())
                            }
                        })
                        .collect();
                let body_results = futures::future::join_all(body_futures).await;
                for result in body_results {
                    result?;
                }
                pb.println(format!(
                    "  Stack section updated in {} PR bodies.",
                    stack_entries.len()
                ));
            }
            // Handled by the cleanup branch above, which runs before any of
            // the rendering setup this arm would not use.
            EffectivePlacement::Cleanup | EffectivePlacement::Ignore => {}
        }
    }

    pb.finish_and_clear();

    // A reconcile failure under `--native-stacks on` fails the submit —
    // reported last, after body syncs and the text placement ran.
    if let Some(e) = native_err {
        return Err(e);
    }

    Ok(SubmissionResult { stack_entries })
}

/// Whether a native server-side stack is in effect for one run.
///
/// Derived from the outcome of the reconcile step, not from a probe — what
/// actually happened on the server, not a prediction about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeState {
    /// The server-side stack was reconciled and now matches the submission.
    Active,
    /// Native stacks are not requested (`ignore`/`none`), definitively
    /// unavailable here, or the submission is a single PR (not a stack).
    Inactive,
    /// The reconcile failed for a reason that says nothing about
    /// availability. Only the destructive placement direction (cleanup)
    /// requires a definitive answer, so auto placements write as usual.
    Unknown,
}

/// The artifact behavior a `StackPlacement` resolves to for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectivePlacement {
    /// Write/update a stack comment on each PR.
    Comment,
    /// Splice the stack into a fenced section of each PR body.
    Body,
    /// Write nothing; retire existing stack comments and body fences.
    Cleanup,
    /// Write nothing and leave existing artifacts untouched.
    Ignore,
}

/// Resolve the requested placement against the native-stack state.
///
/// The auto placements retire stakk's text (cleanup) only on a *definitive*
/// native stack — `Unknown` falls back to writing, because a redundant
/// comment is harmless and self-heals while a skipped update leaves stale
/// stack info standing.
fn resolve_placement(placement: StackPlacement, native: NativeState) -> EffectivePlacement {
    match placement {
        StackPlacement::Comment => EffectivePlacement::Comment,
        StackPlacement::Body => EffectivePlacement::Body,
        StackPlacement::None => EffectivePlacement::Cleanup,
        StackPlacement::Ignore => EffectivePlacement::Ignore,
        StackPlacement::AutoComment => match native {
            NativeState::Active => EffectivePlacement::Cleanup,
            NativeState::Inactive | NativeState::Unknown => EffectivePlacement::Comment,
        },
        StackPlacement::AutoBody => match native {
            NativeState::Active => EffectivePlacement::Cleanup,
            NativeState::Inactive | NativeState::Unknown => EffectivePlacement::Body,
        },
    }
}

/// Converge the forge's server-side stack with the submitted PRs.
///
/// `desired` holds the PR numbers bottom-to-top; the caller guarantees at
/// least two entries. Idempotent: every failure point leaves the server in
/// a state from which a re-run converges.
async fn reconcile_native_stack<F: Forge>(
    forge: &F,
    desired: &[u64],
    pb: &indicatif::ProgressBar,
) -> Result<(), SubmitError> {
    // Query every desired PR, not just the bottom one — an upper PR held by
    // a foreign stack would otherwise make create/add fail opaquely.
    let lookups = futures::future::join_all(desired.iter().map(|&n| forge.get_stacks_for_pr(n)));
    let mut stacks: Vec<ForgeStack> = Vec::new();
    for result in lookups.await {
        for stack in result.map_err(wrap_stack_err)? {
            if !stacks.iter().any(|s| s.number == stack.number) {
                stacks.push(stack);
            }
        }
    }

    match stacks.as_slice() {
        [] => {
            let created = forge.create_stack(desired).await.map_err(wrap_stack_err)?;
            pb.println(format!(
                "  Created server-side stack #{} ({} PRs).",
                created.number,
                desired.len()
            ));
        }
        [stack] if stack.open_pr_numbers == desired => {
            pb.println(format!(
                "  Server-side stack #{} is up to date.",
                stack.number
            ));
        }
        // The server stack extends *above* the submission: the submitted PRs
        // are already its bottom prefix, correctly ordered, and the execute
        // phase never touched the bases of the PRs above them — the chain is
        // intact. Rebuilding here would evict the upper PRs from merge-time
        // retargeting (the empty-diff auto-close hazard the stack exists to
        // prevent), so the stack is left standing. Only a *bottom* prefix is
        // safe to no-op: a slice higher up means the bottom submitted PR was
        // retargeted past open stack members and the chain is broken.
        [stack] if stack.open_pr_numbers.starts_with(desired) => {
            pb.println(format!(
                "  Server-side stack #{} already contains the submission; {} PR(s) above it keep \
                 their stack membership.",
                stack.number,
                stack.open_pr_numbers.len() - desired.len()
            ));
        }
        [stack]
            if !stack.open_pr_numbers.is_empty() && desired.starts_with(&stack.open_pr_numbers) =>
        {
            let suffix = &desired[stack.open_pr_numbers.len()..];
            match forge.add_to_stack(stack.number, suffix).await {
                Ok(_) => {
                    pb.println(format!(
                        "  Added {} PR(s) to server-side stack #{}.",
                        suffix.len(),
                        stack.number
                    ));
                }
                // If the server rejects the append (the add semantics leave
                // room for that — e.g. a PR already held elsewhere),
                // converge via the universal dissolve-and-recreate path.
                Err(ForgeError::StackConflict { .. }) => {
                    forge.unstack(stack.number).await.map_err(wrap_stack_err)?;
                    let created = forge.create_stack(desired).await.map_err(wrap_stack_err)?;
                    pb.println(format!(
                        "  Recreated server-side stack #{} ({} PRs).",
                        created.number,
                        desired.len()
                    ));
                }
                Err(e) => return Err(wrap_stack_err(e)),
            }
        }
        _ => {
            // Reorder, foreign members, multiple stacks, or a stack whose
            // open PRs all merged away: dissolve everything and rebuild.
            // `unstack` removes unmerged PRs wholesale (the API has no
            // per-PR removal); merged leftovers cannot conflict with the
            // new stack. Open PRs that are dissolved along and not part of
            // the new stack lose native stacking silently on GitHub's side,
            // so they are named in a warning.
            let evicted = evicted_pr_numbers(&stacks, desired);
            for stack in &stacks {
                forge.unstack(stack.number).await.map_err(wrap_stack_err)?;
            }
            let created = forge.create_stack(desired).await.map_err(wrap_stack_err)?;
            pb.println(format!(
                "  Recreated server-side stack #{} ({} PRs).",
                created.number,
                desired.len()
            ));
            warn_evicted(&evicted, pb);
        }
    }

    Ok(())
}

/// Dissolve every server-side stack containing a submitted PR.
///
/// `--native-stacks none` mirrors `--stack-placement none`: turning the
/// feature off retires what is standing rather than leaving it stale.
/// Unlike the reconcile this covers single-PR submissions too, and it never
/// fails the submit: `StacksUnavailable` means there is nothing to retire,
/// and any other failure is an advisory warning — a re-run retries.
async fn retire_native_stacks<F: Forge>(forge: &F, submitted: &[u64], pb: &indicatif::ProgressBar) {
    let lookups = futures::future::join_all(submitted.iter().map(|&n| forge.get_stacks_for_pr(n)));
    let mut stacks: Vec<ForgeStack> = Vec::new();
    for result in lookups.await {
        match result {
            Ok(found) => {
                for stack in found {
                    if !stacks.iter().any(|s| s.number == stack.number) {
                        stacks.push(stack);
                    }
                }
            }
            // The feature is not offered here, so nothing can be standing.
            Err(ForgeError::StacksUnavailable { .. }) => return,
            Err(e) => {
                pb.println(format!(
                    "  Warning: could not check for server-side stacks to retire: {e}. Re-running \
                     `stakk submit` retries."
                ));
                return;
            }
        }
    }
    if stacks.is_empty() {
        return;
    }
    // Dissolving a stack unstacks *all* its unmerged members, so open PRs
    // beyond the submitted ones lose native stacking too — name them, like
    // the reconcile's rebuild path does.
    let evicted = evicted_pr_numbers(&stacks, submitted);
    for stack in &stacks {
        match forge.unstack(stack.number).await {
            Ok(()) => {
                pb.println(format!("  Retired server-side stack #{}.", stack.number));
            }
            Err(e) => {
                pb.println(format!(
                    "  Warning: failed to retire server-side stack #{}: {e}. Re-running `stakk \
                     submit` retries.",
                    stack.number
                ));
            }
        }
    }
    warn_evicted(&evicted, pb);
}

/// Open PR numbers that lose their server-side stack membership when
/// `stacks` are dissolved and only `desired` is restacked (or nothing is,
/// for the retirement path).
fn evicted_pr_numbers(stacks: &[ForgeStack], desired: &[u64]) -> Vec<u64> {
    let mut evicted: Vec<u64> = stacks
        .iter()
        .flat_map(|s| s.open_pr_numbers.iter().copied())
        .filter(|n| !desired.contains(n))
        .collect();
    evicted.sort_unstable();
    evicted.dedup();
    evicted
}

/// Warn with the PR numbers that lost native stacking in a dissolve.
fn warn_evicted(evicted: &[u64], pb: &indicatif::ProgressBar) {
    if evicted.is_empty() {
        return;
    }
    let list = evicted
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    pb.println(format!(
        "  Warning: {list} left the dissolved stack(s) and are no longer natively stacked."
    ));
}

/// Remove any stack artifacts (stack comment and body fence) from a single PR.
///
/// Used when stack info is disabled (`StackPlacement::None`) or when a
/// submission no longer forms a stack, to retire stakk's footprint on
/// already-created PRs. Returns `true` if any artifact was found on the PR.
async fn cleanup_stack_artifacts<F: Forge>(
    forge: &F,
    pr_number: u64,
    existing_body: &str,
    pb: &indicatif::ProgressBar,
) -> Result<bool, SubmitError> {
    let mut found = false;

    // Clean up the old stack comment (from comment mode or pre-migration).
    let comments = forge
        .list_comments(pr_number)
        .await
        .map_err(|source| SubmitError::CommentFailed { pr_number, source })?;
    if let Some(old) = find_stack_comment(&comments) {
        found = true;
        if let Err(e) = forge.delete_comment(old.id).await {
            pb.println(format!(
                "  Warning: failed to clean up old stack comment on PR #{pr_number}: {e}"
            ));
        }
    }

    // Clean up the old body fence (from body mode).
    if find_stack_in_body(existing_body).is_some() {
        found = true;
        let stripped = strip_stack_from_body(existing_body);
        if let Err(e) = forge.update_pr_body(pr_number, &stripped).await {
            pb.println(format!(
                "  Warning: failed to strip stack from PR #{pr_number} body: {e}"
            ));
        }
    }

    Ok(found)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;
    use crate::forge::Comment;
    use crate::forge::ForgeError;
    use crate::forge::comment::build_comment_env;
    use crate::graph::types::BranchStack;
    use crate::graph::types::SegmentCommit;
    use crate::jj::JjError;

    // -- Shared operation log for ordering tests --

    type OpLog = Arc<Mutex<Vec<Op>>>;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        CreateBookmark(String),
        Push(String),
        BaseUpdate(u64),
        CreatePr(String),
    }

    // -- Test helpers --

    fn test_comment_env() -> minijinja::Environment<'static> {
        build_comment_env(None).unwrap()
    }

    fn make_segment(names: &[&str], change_id: &str, desc: &str) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(ToString::to_string).collect(),
            change_id: change_id.to_string(),
            commits: vec![SegmentCommit {
                commit_id: format!("c_{change_id}"),
                change_id: change_id.to_string(),
                description: desc.to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: change_id[..4.min(change_id.len())].to_string(),
            }],
        }
    }

    fn make_pr(number: u64, head: &str, base: &str) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://github.com/test/repo/pull/{number}"),
            title: format!("PR for {head}"),
            base_ref: base.to_string(),
            body: None,
        }
    }

    fn make_pr_with_body(number: u64, head: &str, base: &str, body: &str) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://github.com/test/repo/pull/{number}"),
            title: format!("PR for {head}"),
            base_ref: base.to_string(),
            body: Some(body.to_string()),
        }
    }

    // -- Mock Forge --

    struct MockForge {
        existing_prs: HashMap<String, PullRequest>,
        created_prs: Mutex<Vec<CreatePrParams>>,
        created_comments: Mutex<Vec<(u64, String)>>,
        updated_comments: Mutex<Vec<(u64, String)>>,
        updated_bases: Mutex<Vec<(u64, String)>>,
        updated_titles: Mutex<Vec<(u64, String)>>,
        updated_bodies: Mutex<Vec<(u64, String)>>,
        deleted_comments: Mutex<Vec<u64>>,
        existing_comments: HashMap<u64, Vec<Comment>>,
        /// PR numbers `list_comments` was called for, to assert that no
        /// lookup is made on PRs that cannot carry a stack comment.
        listed_comments: Mutex<Vec<u64>>,
        next_pr_number: Mutex<u64>,
        ops: Option<OpLog>,
        existing_stacks: Vec<ForgeStack>,
        /// When set, every stack call fails with `StacksUnavailable` — the
        /// repository does not offer the feature.
        stacks_unavailable: bool,
        /// When set, every stack call fails with a generic `Api` error —
        /// the "not an answer" failure mode.
        stacks_api_error: bool,
        /// When set, `add_to_stack` fails with `StackConflict`.
        add_conflicts: bool,
        /// PR numbers `get_stacks_for_pr` was called for.
        stack_lookups: Mutex<Vec<u64>>,
        created_stacks: Mutex<Vec<Vec<u64>>>,
        added_to_stacks: Mutex<Vec<(u64, Vec<u64>)>>,
        unstacked: Mutex<Vec<u64>>,
        next_stack_number: Mutex<u64>,
    }

    impl MockForge {
        fn new() -> Self {
            Self {
                existing_prs: HashMap::new(),
                created_prs: Mutex::new(Vec::new()),
                created_comments: Mutex::new(Vec::new()),
                updated_comments: Mutex::new(Vec::new()),
                updated_bases: Mutex::new(Vec::new()),
                updated_titles: Mutex::new(Vec::new()),
                updated_bodies: Mutex::new(Vec::new()),
                deleted_comments: Mutex::new(Vec::new()),
                existing_comments: HashMap::new(),
                listed_comments: Mutex::new(Vec::new()),
                next_pr_number: Mutex::new(100),
                ops: None,
                existing_stacks: Vec::new(),
                stacks_unavailable: false,
                stacks_api_error: false,
                add_conflicts: false,
                stack_lookups: Mutex::new(Vec::new()),
                created_stacks: Mutex::new(Vec::new()),
                added_to_stacks: Mutex::new(Vec::new()),
                unstacked: Mutex::new(Vec::new()),
                next_stack_number: Mutex::new(500),
            }
        }

        fn with_ops(mut self, ops: OpLog) -> Self {
            self.ops = Some(ops);
            self
        }

        fn with_existing_pr(mut self, head: &str, pr: PullRequest) -> Self {
            self.existing_prs.insert(head.to_string(), pr);
            self
        }

        fn with_existing_comments(mut self, pr_number: u64, comments: Vec<Comment>) -> Self {
            self.existing_comments.insert(pr_number, comments);
            self
        }

        fn with_existing_stack(mut self, number: u64, open_prs: &[u64]) -> Self {
            self.existing_stacks.push(ForgeStack {
                number,
                open_pr_numbers: open_prs.to_vec(),
            });
            self
        }

        fn with_stacks_unavailable(mut self) -> Self {
            self.stacks_unavailable = true;
            self
        }

        fn with_stacks_api_error(mut self) -> Self {
            self.stacks_api_error = true;
            self
        }

        fn with_add_conflict(mut self) -> Self {
            self.add_conflicts = true;
            self
        }

        /// The configured failure for stack calls, if any.
        fn stack_failure(&self) -> Option<ForgeError> {
            if self.stacks_unavailable {
                return Some(ForgeError::StacksUnavailable {
                    message: "Not Found".to_string(),
                    source: "404".into(),
                });
            }
            if self.stacks_api_error {
                return Some(ForgeError::Api {
                    message: "boom".to_string(),
                    source: "boom".into(),
                });
            }
            None
        }
    }

    impl Forge for MockForge {
        fn find_pr_for_branch(
            &self,
            head: &str,
        ) -> impl std::future::Future<Output = Result<Option<PullRequest>, ForgeError>> + Send
        {
            let result = self.existing_prs.get(head).cloned();
            async move { Ok(result) }
        }

        fn create_pr(
            &self,
            params: CreatePrParams,
        ) -> impl std::future::Future<Output = Result<PullRequest, ForgeError>> + Send {
            let mut counter = self.next_pr_number.lock().unwrap();
            let number = *counter;
            *counter += 1;
            let pr = PullRequest {
                number,
                html_url: format!("https://github.com/test/repo/pull/{number}"),
                title: params.title.clone(),
                base_ref: params.base.clone(),
                body: params.body.clone(),
            };
            if let Some(ops) = &self.ops {
                ops.lock().unwrap().push(Op::CreatePr(params.head.clone()));
            }
            self.created_prs.lock().unwrap().push(params);
            async move { Ok(pr) }
        }

        fn update_pr_base(
            &self,
            pr_number: u64,
            new_base: &str,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            if let Some(ops) = &self.ops {
                ops.lock().unwrap().push(Op::BaseUpdate(pr_number));
            }
            self.updated_bases
                .lock()
                .unwrap()
                .push((pr_number, new_base.to_string()));
            async { Ok(()) }
        }

        fn update_pr_title(
            &self,
            pr_number: u64,
            title: &str,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.updated_titles
                .lock()
                .unwrap()
                .push((pr_number, title.to_string()));
            async { Ok(()) }
        }

        fn list_comments(
            &self,
            pr_number: u64,
        ) -> impl std::future::Future<Output = Result<Vec<Comment>, ForgeError>> + Send {
            self.listed_comments.lock().unwrap().push(pr_number);
            let comments = self
                .existing_comments
                .get(&pr_number)
                .cloned()
                .unwrap_or_default();
            async move { Ok(comments) }
        }

        fn create_comment(
            &self,
            pr_number: u64,
            body: &str,
        ) -> impl std::future::Future<Output = Result<Comment, ForgeError>> + Send {
            let comment = Comment {
                id: pr_number * 1000,
                body: body.to_string(),
            };
            self.created_comments
                .lock()
                .unwrap()
                .push((pr_number, body.to_string()));
            async move { Ok(comment) }
        }

        fn update_comment(
            &self,
            comment_id: u64,
            body: &str,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.updated_comments
                .lock()
                .unwrap()
                .push((comment_id, body.to_string()));
            async { Ok(()) }
        }

        fn update_pr_body(
            &self,
            pr_number: u64,
            body: &str,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.updated_bodies
                .lock()
                .unwrap()
                .push((pr_number, body.to_string()));
            async { Ok(()) }
        }

        fn delete_comment(
            &self,
            comment_id: u64,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.deleted_comments.lock().unwrap().push(comment_id);
            async { Ok(()) }
        }

        fn get_stacks_for_pr(
            &self,
            pr_number: u64,
        ) -> impl std::future::Future<Output = Result<Vec<ForgeStack>, ForgeError>> + Send {
            self.stack_lookups.lock().unwrap().push(pr_number);
            let result = if let Some(e) = self.stack_failure() {
                Err(e)
            } else {
                Ok(self
                    .existing_stacks
                    .iter()
                    .filter(|s| s.open_pr_numbers.contains(&pr_number))
                    .cloned()
                    .collect())
            };
            async move { result }
        }

        fn create_stack(
            &self,
            pr_numbers: &[u64],
        ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send {
            let result = if let Some(e) = self.stack_failure() {
                Err(e)
            } else {
                let mut counter = self.next_stack_number.lock().unwrap();
                let number = *counter;
                *counter += 1;
                self.created_stacks
                    .lock()
                    .unwrap()
                    .push(pr_numbers.to_vec());
                Ok(ForgeStack {
                    number,
                    open_pr_numbers: pr_numbers.to_vec(),
                })
            };
            async move { result }
        }

        fn add_to_stack(
            &self,
            stack_number: u64,
            pr_numbers: &[u64],
        ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send {
            let result = match self.stack_failure() {
                Some(e) => Err(e),
                None if self.add_conflicts => Err(ForgeError::StackConflict {
                    message: "already stacked".to_string(),
                    source: "409".into(),
                }),
                None => {
                    self.added_to_stacks
                        .lock()
                        .unwrap()
                        .push((stack_number, pr_numbers.to_vec()));
                    let mut open_pr_numbers = self
                        .existing_stacks
                        .iter()
                        .find(|s| s.number == stack_number)
                        .map(|s| s.open_pr_numbers.clone())
                        .unwrap_or_default();
                    open_pr_numbers.extend_from_slice(pr_numbers);
                    Ok(ForgeStack {
                        number: stack_number,
                        open_pr_numbers,
                    })
                }
            };
            async move { result }
        }

        fn unstack(
            &self,
            stack_number: u64,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            let result = if let Some(e) = self.stack_failure() {
                Err(e)
            } else {
                self.unstacked.lock().unwrap().push(stack_number);
                Ok(())
            };
            async move { result }
        }
    }

    // -- Mock JjRunner --

    type PushLog = Arc<Mutex<Vec<(String, String)>>>;

    struct MockJjRunner {
        push_calls: PushLog,
        ops: Option<OpLog>,
        /// Local bookmark names the repo reports for `jj bookmark list`.
        local_bookmarks: Vec<String>,
    }

    impl MockJjRunner {
        fn new() -> (Self, PushLog) {
            let calls: PushLog = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    push_calls: Arc::clone(&calls),
                    ops: None,
                    local_bookmarks: Vec::new(),
                },
                calls,
            )
        }

        fn new_with_ops(ops: OpLog) -> (Self, PushLog) {
            let calls: PushLog = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    push_calls: Arc::clone(&calls),
                    ops: Some(ops),
                    local_bookmarks: Vec::new(),
                },
                calls,
            )
        }

        fn with_local_bookmarks(mut self, names: &[&str]) -> Self {
            self.local_bookmarks = names.iter().map(ToString::to_string).collect();
            self
        }
    }

    impl crate::jj::runner::JjRunner for MockJjRunner {
        fn run_jj(
            &self,
            args: &[&str],
        ) -> impl std::future::Future<Output = Result<String, JjError>> + Send {
            let mut output = String::new();
            if args[0] == "bookmark" && args[1] == "list" {
                for name in &self.local_bookmarks {
                    output.push('"');
                    output.push_str(name);
                    output.push_str("\"\n");
                }
            }
            if args[0] == "bookmark"
                && args[1] == "create"
                && let Some(ops) = &self.ops
            {
                ops.lock()
                    .unwrap()
                    .push(Op::CreateBookmark(args[2].to_string()));
            }
            if args[0] == "git" && args[1] == "push" {
                let bookmark = args
                    .iter()
                    .position(|a| *a == "--bookmark")
                    .map(|i| args[i + 1].to_string())
                    .unwrap_or_default();
                let remote = args
                    .iter()
                    .position(|a| *a == "--remote")
                    .map(|i| args[i + 1].to_string())
                    .unwrap_or_default();
                if let Some(ops) = &self.ops {
                    ops.lock().unwrap().push(Op::Push(bookmark.clone()));
                }
                self.push_calls.lock().unwrap().push((bookmark, remote));
            }
            async move { Ok(output) }
        }
    }

    // -----------------------------------------------------------------------
    // Phase 1 tests (analysis_from_selection)
    // -----------------------------------------------------------------------

    fn make_assignment(change_id: &str, name: &str, is_new: bool) -> BookmarkAssignment {
        BookmarkAssignment {
            change_id: change_id.to_string(),
            short_change_id: change_id[..4.min(change_id.len())].to_string(),
            bookmark_name: name.to_string(),
            is_new,
        }
    }

    /// A segment with several commits, described newest-first like the
    /// internal convention. Commit change ids are `<change_id>` for the
    /// boundary and `<change_id>_<i>` for the rest.
    fn make_segment_multi(names: &[&str], change_id: &str, descs: &[&str]) -> BookmarkSegment {
        let mut seg = make_segment(names, change_id, descs[0]);
        let template = seg.commits[0].clone();
        for (i, desc) in descs.iter().enumerate().skip(1) {
            let mut c = template.clone();
            c.change_id = format!("{change_id}_{i}");
            c.commit_id = format!("c_{change_id}_{i}");
            c.short_change_id = format!("{}{i}", template.short_change_id);
            c.description = (*desc).to_string();
            seg.commits.push(c);
        }
        seg
    }

    fn path_of(stack: &BranchStack) -> Vec<SegmentCommit> {
        stack.commits_trunk_to_tip().cloned().collect()
    }

    /// Change IDs of a segment's commits, in stored order.
    fn commit_ids(segment: &BookmarkSegment) -> Vec<&str> {
        segment
            .commits
            .iter()
            .map(|c| c.change_id.as_str())
            .collect()
    }

    /// A leaf-only selection yields one PR whose commits include the folded
    /// ancestor below it.
    #[test]
    fn from_selection_leaf_only_folds_ancestor() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],
        };
        let path = path_of(&stack);

        let direct =
            analysis_from_selection(&path, &[make_assignment("ch_b", "feat-b", false)], "main")
                .unwrap();

        assert_eq!(direct.default_branch, "main");
        assert_eq!(direct.segments.len(), 1);
        assert_eq!(direct.segments[0].bookmark_names, vec!["feat-b"]);
        assert_eq!(direct.segments[0].change_id, "ch_b");
        assert_eq!(commit_ids(&direct.segments[0]), vec!["ch_b", "ch_a"]);
    }

    /// Keeping a subset folds the unkept middle segment into the one above.
    #[test]
    fn from_selection_subset_folds_unkept_middle() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
                make_segment(&["feat-c"], "ch_c", "feature c"),
            ],
        };
        let path = path_of(&stack);

        let direct = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_a", "feat-a", false),
                make_assignment("ch_c", "feat-c", false),
            ],
            "main",
        )
        .unwrap();

        assert_eq!(direct.default_branch, "main");
        assert_eq!(direct.segments.len(), 2);
        assert_eq!(direct.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(direct.segments[0].change_id, "ch_a");
        assert_eq!(commit_ids(&direct.segments[0]), vec!["ch_a"]);
        assert_eq!(direct.segments[1].bookmark_names, vec!["feat-c"]);
        assert_eq!(direct.segments[1].change_id, "ch_c");
        assert_eq!(commit_ids(&direct.segments[1]), vec!["ch_c", "ch_b"]);
    }

    /// Marking every boundary folds nothing — each segment keeps exactly its
    /// own commit, so the leaf and every bookmarked ancestor become separate
    /// stacked PRs rather than one cumulative leaf PR
    /// (<https://github.com/glennib/stakk/issues/184>).
    #[test]
    fn from_selection_all_boundaries_no_folding() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],
        };
        let path = path_of(&stack);

        let direct = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_a", "feat-a", false),
                make_assignment("ch_b", "feat-b", false),
            ],
            "main",
        )
        .unwrap();

        assert_eq!(direct.default_branch, "main");
        assert_eq!(direct.segments.len(), 2);
        assert_eq!(direct.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(commit_ids(&direct.segments[0]), vec!["ch_a"]);
        assert_eq!(direct.segments[1].bookmark_names, vec!["feat-b"]);
        assert_eq!(commit_ids(&direct.segments[1]), vec!["ch_b"]);
    }

    /// A new bookmark on a mid-segment commit splits the segment — possible
    /// without the bookmark existing anywhere yet.
    #[test]
    fn from_selection_splits_segment_at_new_bookmark() {
        let stack = BranchStack {
            segments: vec![make_segment_multi(
                &["feat"],
                "ch_f",
                &["newest work", "older work"],
            )],
        };
        let path = path_of(&stack);

        let direct = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_f_1", "my-new-base", true),
                make_assignment("ch_f", "feat", false),
            ],
            "main",
        )
        .unwrap();

        assert_eq!(direct.segments.len(), 2);
        assert_eq!(direct.segments[0].bookmark_names, vec!["my-new-base"]);
        assert_eq!(direct.segments[0].change_id, "ch_f_1");
        assert_eq!(direct.segments[0].commits.len(), 1);
        assert_eq!(direct.segments[0].commits[0].description, "older work");
        assert_eq!(direct.segments[1].bookmark_names, vec!["feat"]);
        assert_eq!(direct.segments[1].commits.len(), 1);
        assert_eq!(direct.segments[1].commits[0].description, "newest work");
    }

    /// Commits above the topmost assignment are not part of the submission:
    /// the analysis covers trunk up to and including the topmost mark.
    #[test]
    fn from_selection_drops_commits_above_topmost_mark() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],
        };
        let path = path_of(&stack);

        let direct =
            analysis_from_selection(&path, &[make_assignment("ch_a", "feat-a", false)], "main")
                .unwrap();

        assert_eq!(direct.segments.len(), 1);
        assert_eq!(direct.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(direct.segments[0].commits.len(), 1);
    }

    /// Segment commits stay newest-first even when several folded segments
    /// accumulate into one PR.
    #[test]
    fn from_selection_multi_fold_orders_newest_first() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
                make_segment(&["feat-c"], "ch_c", "feature c"),
                make_segment(&["feat-d"], "ch_d", "feature d"),
            ],
        };
        let path = path_of(&stack);

        let direct =
            analysis_from_selection(&path, &[make_assignment("ch_d", "feat-d", false)], "main")
                .unwrap();

        assert_eq!(direct.segments.len(), 1);
        assert_eq!(
            commit_ids(&direct.segments[0]),
            vec!["ch_d", "ch_c", "ch_b", "ch_a"]
        );
    }

    /// An assignment whose change ID is not on the path is a hard error —
    /// silently dropping it would desync the analysis from the bookmark
    /// creations scheduled for execution.
    #[test]
    fn from_selection_errors_on_off_path_assignment() {
        let stack = BranchStack {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],
        };
        let path = path_of(&stack);

        let err = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_a", "feat-a", false),
                make_assignment("ch_elsewhere", "stray", true),
            ],
            "main",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            SubmitError::AssignmentOffPath { ref bookmark, ref change_id }
                if bookmark == "stray" && change_id == "ch_elsewhere"
        ));
    }

    /// A change ID matching two path commits (divergent change) is a hard
    /// error rather than two segments claiming the same bookmark.
    #[test]
    fn from_selection_errors_on_divergent_change() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_dup", "first copy"),
                make_segment(&["feat-b"], "ch_dup", "second copy"),
            ],
        };
        let path = path_of(&stack);

        let err =
            analysis_from_selection(&path, &[make_assignment("ch_dup", "feat-a", false)], "main")
                .unwrap_err();

        assert!(matches!(
            err,
            SubmitError::DivergentChange { ref change_id } if change_id == "ch_dup"
        ));
    }

    /// Empty assignments produce an empty analysis (the callers guard
    /// against submitting one, but the constructor itself must not panic).
    #[test]
    fn from_selection_empty_assignments() {
        let stack = BranchStack {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],
        };
        let path = path_of(&stack);

        let direct = analysis_from_selection(&path, &[], "main").unwrap();
        assert!(direct.segments.is_empty());
        assert_eq!(direct.default_branch, "main");
    }

    // -----------------------------------------------------------------------
    // Phase 2 tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn plan_all_new_prs() {
        let analysis = SubmissionAnalysis {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],

            default_branch: "main".to_string(),
        };

        let forge = MockForge::new();
        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::None,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert_eq!(plan.bookmark_plans.len(), 2);

        assert!(plan.bookmark_plans[0].needs_create);
        assert!(!plan.bookmark_plans[0].needs_base_update);
        assert_eq!(plan.bookmark_plans[0].base, "main");

        assert!(plan.bookmark_plans[1].needs_create);
        assert!(!plan.bookmark_plans[1].needs_base_update);
        assert_eq!(plan.bookmark_plans[1].base, "feat-a");
    }

    #[tokio::test]
    async fn plan_existing_pr_correct_base() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],

            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr("feat-a", make_pr(42, "feat-a", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::None,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_create);
        assert!(!plan.bookmark_plans[0].needs_base_update);
        assert_eq!(
            plan.bookmark_plans[0].existing_pr.as_ref().unwrap().number,
            42
        );
    }

    #[tokio::test]
    async fn plan_existing_pr_wrong_base() {
        let analysis = SubmissionAnalysis {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],

            default_branch: "main".to_string(),
        };

        let forge = MockForge::new()
            .with_existing_pr("feat-a", make_pr(10, "feat-a", "main"))
            .with_existing_pr("feat-b", make_pr(11, "feat-b", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::None,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        // feat-a: base is "main", existing PR base is "main" -> no update
        assert!(!plan.bookmark_plans[0].needs_base_update);

        // feat-b: base should be "feat-a", existing PR base is "main" ->
        // needs update
        assert!(plan.bookmark_plans[1].needs_base_update);
        assert_eq!(plan.bookmark_plans[1].base, "feat-a");
    }

    #[tokio::test]
    async fn plan_mixed_existing_and_new() {
        let analysis = SubmissionAnalysis {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],

            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr("feat-a", make_pr(10, "feat-a", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::None,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_create);
        assert!(plan.bookmark_plans[1].needs_create);
    }

    #[tokio::test]
    async fn plan_sync_title_detects_change() {
        let analysis = SubmissionAnalysis {
            // Commit title "feature a" differs from PR title "PR for feat-a".
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr("feat-a", make_pr(42, "feat-a", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::All,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(plan.bookmark_plans[0].needs_title_sync);
        // Body is None for both commit and PR — no body sync needed.
        assert!(!plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_title_skips_when_same() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "PR for feat-a")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr("feat-a", make_pr(42, "feat-a", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::All,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_title_sync);
    }

    #[tokio::test]
    async fn plan_sync_body_detects_change() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(
                &["feat-a"],
                "ch_a",
                "feature a\n\nnew body text",
            )],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr(
            "feat-a",
            make_pr_with_body(42, "feat-a", "main", "old body"),
        );

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::All,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_body_ignores_fenced_section() {
        let fenced_body =
            "old body\n\n<!-- STAKK_BODY_START -->\nstack info\n<!-- STAKK_BODY_END -->";
        let analysis = SubmissionAnalysis {
            // Commit body = "old body" matches the non-fenced portion.
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a\n\nold body")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr(
            "feat-a",
            make_pr_with_body(42, "feat-a", "main", fenced_body),
        );

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::All,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_disabled_does_not_set_flags() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr("feat-a", make_pr(42, "feat-a", "main"));

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::None,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_title_sync);
        assert!(!plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_new_pr_does_not_set_flags() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new();

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::All,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_title_sync);
        assert!(!plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_title_only_does_not_set_body_flag() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a\n\nnew body")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr(
            "feat-a",
            make_pr_with_body(42, "feat-a", "main", "old body"),
        );

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::Title,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(plan.bookmark_plans[0].needs_title_sync);
        assert!(!plan.bookmark_plans[0].needs_body_sync);
    }

    #[tokio::test]
    async fn plan_sync_body_only_does_not_set_title_flag() {
        let analysis = SubmissionAnalysis {
            segments: vec![make_segment(&["feat-a"], "ch_a", "feature a\n\nnew body")],
            default_branch: "main".to_string(),
        };

        let forge = MockForge::new().with_existing_pr(
            "feat-a",
            make_pr_with_body(42, "feat-a", "main", "old body"),
        );

        let plan = create_submission_plan(
            &analysis,
            vec![],
            &forge,
            "origin",
            PrMode::Regular,
            SyncPrContent::Body,
            TrailerHandling::Keep,
        )
        .await
        .unwrap();

        assert!(!plan.bookmark_plans[0].needs_title_sync);
        assert!(plan.bookmark_plans[0].needs_body_sync);
    }

    #[test]
    fn plan_display_dry_run() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(42, "feat-b", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(output.contains("2 bookmark(s)"));
        assert!(output.contains("feat-a (base: main)"));
        assert!(output.contains("create PR: \"feature a\""));
        assert!(output.contains("push bookmark to origin"));
        assert!(output.contains("update PR #42 base: main -> feat-a"));
    }

    #[test]
    fn plan_display_shows_sync_lines() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: Some("body text".to_string()),
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: true,
                needs_body_sync: true,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(output.contains("sync PR #42 title from commits"));
        assert!(output.contains("sync PR #42 body from commits"));
        assert!(!output.contains("up to date"));
    }

    #[test]
    fn plan_display_shows_pending_bookmark_creations() {
        let plan = SubmissionPlan {
            bookmark_creations: vec![BookmarkCreation {
                bookmark_name: "my-feature".to_string(),
                change_id: "ch_new_full_id".to_string(),
                short_change_id: "ch_n".to_string(),
            }],
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "my-feature".to_string(),
                base: "main".to_string(),
                title: "my feature".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(output.contains("Create bookmark my-feature at ch_n"));
        // Creations are listed before the per-bookmark plans.
        let creation_pos = output.find("Create bookmark").unwrap();
        let plan_pos = output.find("my-feature (base: main)").unwrap();
        assert!(creation_pos < plan_pos);
    }

    // -----------------------------------------------------------------------
    // Phase 3 tests
    // -----------------------------------------------------------------------

    /// Bookmark creations happen before any push or PR creation — pushes
    /// require the bookmark to exist locally.
    #[tokio::test]
    async fn execute_creates_bookmarks_before_pushing() {
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));
        let plan = SubmissionPlan {
            bookmark_creations: vec![BookmarkCreation {
                bookmark_name: "feat-new".to_string(),
                change_id: "ch_new".to_string(),
                short_change_id: "ch_n".to_string(),
            }],
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-new".to_string(),
                base: "main".to_string(),
                title: "new feature".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let ops = ops.lock().unwrap();
        assert_eq!(
            *ops,
            vec![
                Op::CreateBookmark("feat-new".to_string()),
                Op::Push("feat-new".to_string()),
                Op::CreatePr("feat-new".to_string()),
            ],
        );
    }

    /// A name taken since selection aborts the run before anything is
    /// created or pushed — no half-applied plan.
    #[tokio::test]
    async fn execute_rejects_taken_bookmark_names_before_mutating() {
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));
        let plan = SubmissionPlan {
            bookmark_creations: vec![
                BookmarkCreation {
                    bookmark_name: "feat-free".to_string(),
                    change_id: "ch_a".to_string(),
                    short_change_id: "ch_a".to_string(),
                },
                BookmarkCreation {
                    bookmark_name: "feat-taken".to_string(),
                    change_id: "ch_b".to_string(),
                    short_change_id: "ch_b".to_string(),
                },
            ],
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-free".to_string(),
                base: "main".to_string(),
                title: "new feature".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        // "feat-taken" appeared between selection and execution.
        let jj = Jj::new(runner.with_local_bookmarks(&["main", "feat-taken"]));
        let forge = MockForge::new().with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        let err = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, SubmitError::BookmarkNamesTaken { bookmarks } if bookmarks == &["feat-taken"]),
            "unexpected error: {err:?}",
        );
        // The earlier, still-free bookmark must not have been created.
        assert!(ops.lock().unwrap().is_empty(), "repo was mutated: {ops:?}");
    }

    /// Names that are free pass the pre-flight check untouched.
    #[tokio::test]
    async fn execute_accepts_free_bookmark_names() {
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));
        let plan = SubmissionPlan {
            bookmark_creations: vec![BookmarkCreation {
                bookmark_name: "feat-new".to_string(),
                change_id: "ch_new".to_string(),
                short_change_id: "ch_n".to_string(),
            }],
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-new".to_string(),
                base: "main".to_string(),
                title: "new feature".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        let jj = Jj::new(runner.with_local_bookmarks(&["main", "other"]));
        let forge = MockForge::new().with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        assert_eq!(
            *ops.lock().unwrap(),
            vec![
                Op::CreateBookmark("feat-new".to_string()),
                Op::Push("feat-new".to_string()),
                Op::CreatePr("feat-new".to_string()),
            ],
        );
    }

    #[tokio::test]
    async fn execute_creates_new_prs() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        let result = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        assert_eq!(result.stack_entries.len(), 2);

        let created = forge.created_prs.lock().unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].head, "feat-a");
        assert_eq!(created[0].base, "main");
        assert_eq!(created[1].head, "feat-b");
        assert_eq!(created[1].base, "feat-a");
    }

    #[tokio::test]
    async fn execute_updates_base() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "develop".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: true,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated = forge.updated_bases.lock().unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0], (42, "develop".to_string()));
    }

    #[tokio::test]
    async fn execute_creates_stack_comments() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let comments = forge.created_comments.lock().unwrap();
        // One stack comment per PR.
        assert_eq!(comments.len(), 2);
        // Comments should contain STAKK_STACK metadata.
        assert!(comments[0].1.contains("STAKK_STACK"));
        assert!(comments[1].1.contains("STAKK_STACK"));
    }

    #[tokio::test]
    async fn execute_updates_existing_stack_comments() {
        let env = test_comment_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let existing_comment_body = format_stack_comment(
            &StackCommentData {
                version: 0,
                stack: vec![StackEntry {
                    bookmark_name: "old".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 1,
                }],
            },
            &StackCommentContext {
                stack: vec![StackEntryContext {
                    bookmark_name: "old".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 1,
                    title: "old feature".to_string(),
                    base: "main".to_string(),
                    is_draft: false,
                    position: 1,
                    is_current: true,
                    is_leaf: true,
                }],
                stack_size: 1,
                default_branch: "main".to_string(),
                current_bookmark: "old".to_string(),
                stakk_url: STAKK_REPO_URL.to_string(),
            },
            &tmpl,
        )
        .unwrap();

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(50, "feat-a", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: existing_comment_body,
            }],
        );

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Should have updated the existing comment on PR #50, not created a
        // new one. A new comment is created for the second PR.
        let created = forge.created_comments.lock().unwrap();
        assert_eq!(created.len(), 1);

        let updated = forge.updated_comments.lock().unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].0, 999);
    }

    #[tokio::test]
    async fn execute_pushes_bookmarks() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "my-remote".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let calls = push_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], ("feat-a".to_string(), "my-remote".to_string()));
        assert_eq!(calls[1], ("feat-b".to_string(), "my-remote".to_string()));
    }

    #[test]
    fn plan_display_shows_draft() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Draft,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(
            output.contains("draft"),
            "expected 'draft' in plan display: {output}"
        );
    }

    #[tokio::test]
    async fn execute_creates_draft_prs() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Draft,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let created = forge.created_prs.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].draft, "expected PR to be created as draft");
    }

    #[tokio::test]
    async fn execute_syncs_title_and_body_for_existing_pr() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "updated title".to_string(),
                body: Some("updated body".to_string()),
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: true,
                needs_body_sync: true,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated_titles = forge.updated_titles.lock().unwrap();
        assert_eq!(updated_titles.len(), 1);
        assert_eq!(updated_titles[0], (42, "updated title".to_string()));

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 1);
        assert_eq!(updated_bodies[0], (42, "updated body".to_string()));
    }

    #[tokio::test]
    async fn execute_syncs_clears_body_when_no_commit_body() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "title only".to_string(),
                body: None,
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: true,
                needs_body_sync: true,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated_titles = forge.updated_titles.lock().unwrap();
        assert_eq!(updated_titles[0], (42, "title only".to_string()));

        // Body sync with None body clears the body to empty string.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies[0], (42, String::new()));
    }

    #[tokio::test]
    async fn execute_no_sync_when_flag_off() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: Some("body".to_string()),
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated_titles = forge.updated_titles.lock().unwrap();
        assert!(updated_titles.is_empty());
    }

    #[tokio::test]
    async fn execute_body_mode_sync_uses_commit_body_for_fence() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: Some("new commit body".to_string()),
                    existing_pr: Some(make_pr(10, "feat-a", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: true,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: Some("commit body b".to_string()),
                    existing_pr: Some(make_pr(11, "feat-b", "feat-a")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: true,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Body,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Body sync is skipped in the per-bookmark loop when body-mode is
        // active — the fence-splicing phase handles it in a single API call.
        // So there should be exactly 2 body updates (one per PR), not 4.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        // Both bodies should contain the commit text and the STAKK_BODY_START
        // fence.
        assert!(updated_bodies[0].1.contains("new commit body"));
        assert!(updated_bodies[0].1.contains("STAKK_BODY_START"));
        assert!(updated_bodies[1].1.contains("commit body b"));
        assert!(updated_bodies[1].1.contains("STAKK_BODY_START"));
    }

    // -----------------------------------------------------------------------
    // build_pr_body tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_pr_body_single_commit_with_body() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X\n\nThis adds feature X with foo and bar.".to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(
            body.as_deref(),
            Some("This adds feature X with foo and bar.")
        );
    }

    #[test]
    fn build_pr_body_single_commit_title_only() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X".to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(body, None);
    }

    #[test]
    fn build_pr_body_multiple_commits() {
        let commits = vec![
            SegmentCommit {
                commit_id: "c1".to_string(),
                change_id: "ch1".to_string(),
                description: "First commit".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch1".to_string(),
            },
            SegmentCommit {
                commit_id: "c2".to_string(),
                change_id: "ch2".to_string(),
                description: "Second commit".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch2".to_string(),
            },
        ];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(
            body.as_deref(),
            Some("First commit\n\n---\n\nSecond commit")
        );
    }

    #[test]
    fn build_pr_body_empty() {
        let body = build_pr_body(&[], TrailerHandling::Keep);
        assert_eq!(body, None);
    }

    #[test]
    fn build_pr_body_single_commit_strips_trailers() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X\n\nThis adds feature X.\n\nSigned-off-by: Alice \
                          <a@b>\nRefs: DAT-123"
                .to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Strip);
        assert_eq!(body.as_deref(), Some("This adds feature X."));
    }

    #[test]
    fn build_pr_body_single_commit_title_plus_trailers_only() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X\n\nSigned-off-by: Alice <a@b>".to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Strip);
        assert_eq!(body, None);
    }

    #[test]
    fn build_pr_body_multiple_commits_strips_trailers() {
        let commits = vec![
            SegmentCommit {
                commit_id: "c1".to_string(),
                change_id: "ch1".to_string(),
                description: "First commit\n\nSigned-off-by: Alice <a@b>".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch1".to_string(),
            },
            SegmentCommit {
                commit_id: "c2".to_string(),
                change_id: "ch2".to_string(),
                description: "Second commit\n\nWith a body.\n\nRefs: DAT-456".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch2".to_string(),
            },
        ];

        let body = build_pr_body(&commits, TrailerHandling::Strip);
        assert_eq!(
            body.as_deref(),
            Some("First commit\n\n---\n\nSecond commit\n\nWith a body.")
        );
    }

    #[test]
    fn build_pr_body_single_commit_keeps_trailers() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X\n\nThis adds feature X.\n\nSigned-off-by: Alice \
                          <a@b>\nRefs: DAT-123"
                .to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(
            body.as_deref(),
            Some("This adds feature X.\n\nSigned-off-by: Alice <a@b>\nRefs: DAT-123")
        );
    }

    #[test]
    fn build_pr_body_single_commit_title_plus_trailers_only_kept() {
        let commits = vec![SegmentCommit {
            commit_id: "c1".to_string(),
            change_id: "ch1".to_string(),
            description: "Add feature X\n\nSigned-off-by: Alice <a@b>".to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            committer: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            is_immutable: false,
            local_bookmark_names: vec![],
            remote_bookmark_names: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(body.as_deref(), Some("Signed-off-by: Alice <a@b>"));
    }

    #[test]
    fn build_pr_body_multiple_commits_keeps_trailers() {
        let commits = vec![
            SegmentCommit {
                commit_id: "c1".to_string(),
                change_id: "ch1".to_string(),
                description: "First commit\n\nSigned-off-by: Alice <a@b>".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch1".to_string(),
            },
            SegmentCommit {
                commit_id: "c2".to_string(),
                change_id: "ch2".to_string(),
                description: "Second commit\n\nWith a body.\n\nRefs: DAT-456".to_string(),
                author: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                committer: crate::jj::types::Signature {
                    name: "Test".to_string(),
                    email: "test@test.com".to_string(),
                    timestamp: "T".to_string(),
                },
                files: vec![],
                is_immutable: false,
                local_bookmark_names: vec![],
                remote_bookmark_names: vec![],
                short_change_id: "ch2".to_string(),
            },
        ];

        let body = build_pr_body(&commits, TrailerHandling::Keep);
        assert_eq!(
            body.as_deref(),
            Some(
                "First commit\n\nSigned-off-by: Alice <a@b>\n\n---\n\nSecond commit\n\nWith a \
                 body.\n\nRefs: DAT-456"
            )
        );
    }

    // -----------------------------------------------------------------------
    // Body placement tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_body_mode_creates_fenced_section() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Body,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        assert!(
            updated_bodies[0].1.contains("STAKK_BODY_START"),
            "expected body fence: {}",
            updated_bodies[0].1
        );
        assert!(
            updated_bodies[0].1.contains("STAKK_STACK"),
            "expected stack metadata in body: {}",
            updated_bodies[0].1
        );

        // No comment API calls should be made in steady-state body mode.
        let created_comments = forge.created_comments.lock().unwrap();
        assert_eq!(created_comments.len(), 0);
    }

    #[tokio::test]
    async fn execute_body_mode_updates_existing_fence() {
        use crate::forge::comment::splice_stack_into_body;

        let existing_body = splice_stack_into_body("Original PR body", "old stack content");
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr_with_body(50, "feat-a", "main", &existing_body)),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Body,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        // PR #50 (feat-a) should still contain original body text.
        assert!(updated_bodies[0].1.contains("Original PR body"));
        // Should no longer contain old stack content.
        assert!(!updated_bodies[0].1.contains("old stack content"));
        // Should contain new STAKK_STACK metadata.
        assert!(updated_bodies[0].1.contains("STAKK_STACK"));

        // No comment API calls (existing fence = not first time for feat-a,
        // new PR for feat-b has no old comment either).
        let created_comments = forge.created_comments.lock().unwrap();
        assert_eq!(created_comments.len(), 0);
        let deleted = forge.deleted_comments.lock().unwrap();
        assert_eq!(deleted.len(), 0);
    }

    #[tokio::test]
    async fn execute_body_mode_migration_deletes_old_comment() {
        // Simulate a PR that has an old stack comment but no body fence.
        let env = test_comment_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let old_comment_body = format_stack_comment(
            &StackCommentData {
                version: 0,
                stack: vec![StackEntry {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                }],
            },
            &StackCommentContext {
                stack: vec![StackEntryContext {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                    title: "feature a".to_string(),
                    base: "main".to_string(),
                    is_draft: false,
                    position: 1,
                    is_current: true,
                    is_leaf: true,
                }],
                stack_size: 1,
                default_branch: "main".to_string(),
                current_bookmark: "feat-a".to_string(),
                stakk_url: STAKK_REPO_URL.to_string(),
            },
            &tmpl,
        )
        .unwrap();

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr_with_body(50, "feat-a", "main", "Plain body")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: old_comment_body,
            }],
        );

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Body,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Should have written body for both PRs.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        assert!(updated_bodies[0].1.contains("STAKK_BODY_START"));

        // Should have deleted the old comment on PR #50 (migration).
        let deleted = forge.deleted_comments.lock().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], 999);
    }

    #[tokio::test]
    async fn execute_comment_mode_migration_strips_body() {
        use crate::forge::comment::splice_stack_into_body;

        // PR has a fenced section in the body (from previous body mode).
        let body_with_fence = splice_stack_into_body("Original PR body", "old stack content");
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr_with_body(50, "feat-a", "main", &body_with_fence)),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        // No existing stack comment — so it will create one, triggering
        // migration check.
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Should have created comments for both PRs.
        let created_comments = forge.created_comments.lock().unwrap();
        assert_eq!(created_comments.len(), 2);
        assert!(created_comments[0].1.contains("STAKK_STACK"));

        // Should have stripped the fence from the body of PR #50 (migration).
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 1);
        assert!(
            !updated_bodies[0].1.contains("STAKK_BODY_START"),
            "fence should be stripped: {}",
            updated_bodies[0].1
        );
        assert!(updated_bodies[0].1.contains("Original PR body"));
    }

    // -----------------------------------------------------------------------
    // Single-bookmark (no stack info) tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_single_bookmark_skips_stack_comment() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: None,
                needs_push: true,
                needs_create: true,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        let result = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        assert_eq!(result.stack_entries.len(), 1);

        // PR should be created.
        let created_prs = forge.created_prs.lock().unwrap();
        assert_eq!(created_prs.len(), 1);

        // No stack comments should be created.
        let created_comments = forge.created_comments.lock().unwrap();
        assert_eq!(created_comments.len(), 0);

        // No body updates for stack info.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 0);
    }

    #[tokio::test]
    async fn execute_single_bookmark_cleans_up_old_comment() {
        let env = test_comment_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let old_comment_body = format_stack_comment(
            &StackCommentData {
                version: 0,
                stack: vec![StackEntry {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                }],
            },
            &StackCommentContext {
                stack: vec![StackEntryContext {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                    title: "feature a".to_string(),
                    base: "main".to_string(),
                    is_draft: false,
                    position: 1,
                    is_current: true,
                    is_leaf: true,
                }],
                stack_size: 1,
                default_branch: "main".to_string(),
                current_bookmark: "feat-a".to_string(),
                stakk_url: STAKK_REPO_URL.to_string(),
            },
            &tmpl,
        )
        .unwrap();

        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: Some(make_pr(50, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: old_comment_body,
            }],
        );

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Old stack comment should be deleted.
        let deleted = forge.deleted_comments.lock().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], 999);

        // No new comments should be created.
        let created = forge.created_comments.lock().unwrap();
        assert_eq!(created.len(), 0);
    }

    #[tokio::test]
    async fn execute_single_bookmark_cleans_up_old_body_fence() {
        use crate::forge::comment::splice_stack_into_body;

        let body_with_fence = splice_stack_into_body("Original PR body", "old stack content");
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: Some(make_pr_with_body(50, "feat-a", "main", &body_with_fence)),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Body,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Body fence should be stripped.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 1);
        assert!(
            !updated_bodies[0].1.contains("STAKK_BODY_START"),
            "fence should be stripped: {}",
            updated_bodies[0].1
        );
        assert!(updated_bodies[0].1.contains("Original PR body"));

        // No new comments should be created.
        let created = forge.created_comments.lock().unwrap();
        assert_eq!(created.len(), 0);
    }

    // -----------------------------------------------------------------------
    // None placement (stack info disabled) tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_none_placement_writes_no_comments_or_bodies() {
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();
        let env = test_comment_env();

        let result = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::None,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        assert_eq!(result.stack_entries.len(), 2);

        // PRs should be created...
        let created_prs = forge.created_prs.lock().unwrap();
        assert_eq!(created_prs.len(), 2);

        // ...but no stack comments and no body updates.
        let created_comments = forge.created_comments.lock().unwrap();
        assert_eq!(created_comments.len(), 0);
        let updated_comments = forge.updated_comments.lock().unwrap();
        assert_eq!(updated_comments.len(), 0);
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 0);

        // Both PRs were created in this run, so neither can carry a stack
        // comment — cleanup must not spend an API call looking.
        let listed = forge.listed_comments.lock().unwrap();
        assert!(listed.is_empty(), "unexpected comment lookups: {listed:?}");
    }

    #[tokio::test]
    async fn execute_none_placement_cleans_up_existing_artifacts() {
        use crate::forge::comment::splice_stack_into_body;

        // PR #50 carries both an old stack comment and a body fence.
        let body_with_fence = splice_stack_into_body("Original PR body", "old stack content");
        let env = test_comment_env();
        let tmpl = env.get_template("stack_comment").unwrap();
        let old_comment_body = format_stack_comment(
            &StackCommentData {
                version: 0,
                stack: vec![StackEntry {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                }],
            },
            &StackCommentContext {
                stack: vec![StackEntryContext {
                    bookmark_name: "feat-a".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 50,
                    title: "feature a".to_string(),
                    base: "main".to_string(),
                    is_draft: false,
                    position: 1,
                    is_current: true,
                    is_leaf: true,
                }],
                stack_size: 1,
                default_branch: "main".to_string(),
                current_bookmark: "feat-a".to_string(),
                stakk_url: STAKK_REPO_URL.to_string(),
            },
            &tmpl,
        )
        .unwrap();

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr_with_body(50, "feat-a", "main", &body_with_fence)),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: old_comment_body,
            }],
        );

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::None,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // Old stack comment on PR #50 should be deleted.
        let deleted = forge.deleted_comments.lock().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], 999);

        // Body fence on PR #50 should be stripped.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 1);
        assert!(
            !updated_bodies[0].1.contains("STAKK_BODY_START"),
            "fence should be stripped: {}",
            updated_bodies[0].1
        );
        assert!(updated_bodies[0].1.contains("Original PR body"));

        // No new comments should be created.
        let created = forge.created_comments.lock().unwrap();
        assert_eq!(created.len(), 0);

        // Only the pre-existing PR is inspected; feat-b was created in this
        // run and is skipped.
        let listed = forge.listed_comments.lock().unwrap();
        assert_eq!(*listed, vec![50]);
    }

    // -- ignore placement --

    #[tokio::test]
    async fn execute_ignore_placement_touches_no_artifacts() {
        use crate::forge::comment::splice_stack_into_body;

        // PR #50 carries both an old stack comment and a body fence; ignore
        // must neither update nor remove them.
        let body_with_fence = splice_stack_into_body("Original PR body", "old stack content");
        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr_with_body(50, "feat-a", "main", &body_with_fence)),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: "<!--- STAKK_STACK: e30= --->\nold stack comment".to_string(),
            }],
        );
        let env = test_comment_env();

        let result = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Ignore,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        // The submission itself runs normally...
        assert_eq!(result.stack_entries.len(), 2);
        assert_eq!(forge.created_prs.lock().unwrap().len(), 1);
        // ...but no artifact is written, updated, or removed — not even a
        // comment lookup.
        assert!(forge.created_comments.lock().unwrap().is_empty());
        assert!(forge.updated_comments.lock().unwrap().is_empty());
        assert!(forge.updated_bodies.lock().unwrap().is_empty());
        assert!(forge.deleted_comments.lock().unwrap().is_empty());
        assert!(forge.listed_comments.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_ignore_single_bookmark_skips_cleanup() {
        // Unlike other placements, a single-bookmark submission with ignore
        // does not retire stale artifacts.
        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                body: None,
                existing_pr: Some(make_pr(50, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
                needs_title_sync: false,
                needs_body_sync: false,
            }],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: "<!--- STAKK_STACK: e30= --->\nold stack comment".to_string(),
            }],
        );
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Ignore,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        assert!(forge.deleted_comments.lock().unwrap().is_empty());
        assert!(forge.listed_comments.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_interleaves_push_and_base_update() {
        // Two existing PRs both needing push + base update (simulates a swap).
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(10, "feat-a", "feat-b")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(11, "feat-b", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Draft,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        let jj = Jj::new(runner);
        let forge = MockForge::new()
            .with_existing_pr("feat-a", make_pr(10, "feat-a", "feat-b"))
            .with_existing_pr("feat-b", make_pr(11, "feat-b", "main"))
            .with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let ops = ops.lock().unwrap();
        assert_eq!(
            *ops,
            vec![
                Op::Push("feat-a".to_string()),
                Op::BaseUpdate(10),
                Op::Push("feat-b".to_string()),
                Op::BaseUpdate(11),
            ],
            "each bookmark must be pushed and have its base updated before the next bookmark is \
             pushed (prevents transient empty diffs)"
        );
    }

    #[tokio::test]
    async fn execute_interleaves_three_bookmark_reorder() {
        // Three existing PRs all needing push + base update.
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(10, "feat-a", "feat-c")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(11, "feat-b", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-c".to_string(),
                    base: "feat-b".to_string(),
                    title: "feature c".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(12, "feat-c", "feat-a")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Draft,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        let jj = Jj::new(runner);
        let forge = MockForge::new()
            .with_existing_pr("feat-a", make_pr(10, "feat-a", "feat-c"))
            .with_existing_pr("feat-b", make_pr(11, "feat-b", "main"))
            .with_existing_pr("feat-c", make_pr(12, "feat-c", "feat-a"))
            .with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let ops = ops.lock().unwrap();
        assert_eq!(
            *ops,
            vec![
                Op::Push("feat-a".to_string()),
                Op::BaseUpdate(10),
                Op::Push("feat-b".to_string()),
                Op::BaseUpdate(11),
                Op::Push("feat-c".to_string()),
                Op::BaseUpdate(12),
            ],
            "strict interleaving: push(i), update(i), push(i+1), update(i+1), ..."
        );
    }

    #[tokio::test]
    async fn execute_interleaves_push_update_and_create() {
        // First bookmark has existing PR needing base update, second is new.
        let ops: OpLog = Arc::new(Mutex::new(Vec::new()));

        let plan = SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(10, "feat-a", "feat-b")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Draft,
            default_branch: "main".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new_with_ops(Arc::clone(&ops));
        let jj = Jj::new(runner);
        let forge = MockForge::new()
            .with_existing_pr("feat-a", make_pr(10, "feat-a", "feat-b"))
            .with_ops(Arc::clone(&ops));
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::Ignore,
        )
        .await
        .unwrap();

        let ops = ops.lock().unwrap();
        assert_eq!(
            *ops,
            vec![
                Op::Push("feat-a".to_string()),
                Op::BaseUpdate(10),
                Op::Push("feat-b".to_string()),
                Op::CreatePr("feat-b".to_string()),
            ],
            "base update for feat-a must complete before feat-b is pushed"
        );
    }

    // -----------------------------------------------------------------------
    // Native stack reconciliation tests
    // -----------------------------------------------------------------------

    /// A two-PR plan whose PRs already exist (#41 bottom, #42 top), so PR
    /// numbers are stable for stack assertions.
    fn two_existing_pr_plan() -> SubmissionPlan {
        SubmissionPlan {
            bookmark_plans: vec![
                BookmarkPlan {
                    bookmark_name: "feat-a".to_string(),
                    base: "main".to_string(),
                    title: "feature a".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(41, "feat-a", "main")),
                    needs_push: false,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    body: None,
                    existing_pr: Some(make_pr(42, "feat-b", "feat-a")),
                    needs_push: false,
                    needs_create: false,
                    needs_base_update: false,
                    needs_title_sync: false,
                    needs_body_sync: false,
                },
            ],
            bookmark_creations: vec![],
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        }
    }

    async fn run_plan(
        plan: &SubmissionPlan,
        forge: &MockForge,
        placement: StackPlacement,
        native: NativeStacks,
    ) -> Result<SubmissionResult, SubmitError> {
        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let env = test_comment_env();
        execute_submission_plan(plan, &jj, forge, &env, placement, native).await
    }

    #[test]
    fn resolve_placement_matrix() {
        use EffectivePlacement as E;
        use NativeState as N;
        use StackPlacement as P;

        // Literal placements ignore the native state entirely.
        for native in [N::Active, N::Inactive, N::Unknown] {
            assert_eq!(resolve_placement(P::Comment, native), E::Comment);
            assert_eq!(resolve_placement(P::Body, native), E::Body);
            assert_eq!(resolve_placement(P::None, native), E::Cleanup);
            assert_eq!(resolve_placement(P::Ignore, native), E::Ignore);
        }

        // Auto placements retire the text only on a *definitive* native
        // stack; an unknown state falls back to writing, never to silence —
        // a redundant comment self-heals, a stale one misleads.
        assert_eq!(resolve_placement(P::AutoComment, N::Active), E::Cleanup);
        assert_eq!(resolve_placement(P::AutoComment, N::Inactive), E::Comment);
        assert_eq!(resolve_placement(P::AutoComment, N::Unknown), E::Comment);
        assert_eq!(resolve_placement(P::AutoBody, N::Active), E::Cleanup);
        assert_eq!(resolve_placement(P::AutoBody, N::Inactive), E::Body);
        assert_eq!(resolve_placement(P::AutoBody, N::Unknown), E::Body);
    }

    #[tokio::test]
    async fn native_ignore_never_touches_the_stack_api() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[41, 42]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::Ignore)
            .await
            .unwrap();

        assert!(forge.stack_lookups.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    /// `none` mirrors `--stack-placement none`: it registers nothing and
    /// retires the server-side stacks that are standing.
    #[tokio::test]
    async fn native_none_retires_existing_stacks() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[41, 42]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::None)
            .await
            .unwrap();

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![7]);
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
        // Inactive native state: the requested placement still writes.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
    }

    /// Unlike the reconcile, the retirement covers single-PR submissions:
    /// a stale stack containing the one PR is exactly what `none` retires.
    #[tokio::test]
    async fn native_none_retires_stacks_for_single_pr_submissions() {
        let mut plan = two_existing_pr_plan();
        plan.bookmark_plans.truncate(1);
        let forge = MockForge::new().with_existing_stack(7, &[41, 43]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::None)
            .await
            .unwrap();

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![7]);
    }

    /// Where the feature is not offered nothing can be standing — `none`
    /// skips silently instead of failing.
    #[tokio::test]
    async fn native_none_skips_silently_when_unavailable() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_stacks_unavailable();
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::None)
            .await
            .unwrap();

        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    /// A transient failure while retiring is an advisory warning, never a
    /// failed submit — a re-run retries.
    #[tokio::test]
    async fn native_none_transient_error_does_not_fail_the_submit() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_stacks_api_error();
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::None)
            .await
            .unwrap();

        assert!(forge.unstacked.lock().unwrap().is_empty());
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
    }

    /// One PR is not a stack: even `on` skips the stack API entirely.
    #[tokio::test]
    async fn native_on_single_pr_skips_the_stack_api() {
        let mut plan = two_existing_pr_plan();
        plan.bookmark_plans.truncate(1);
        let forge = MockForge::new();
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert!(forge.stack_lookups.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn native_on_creates_a_stack_when_none_exists() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new();
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        // Every desired PR is queried, not only the bottom one.
        assert_eq!(*forge.stack_lookups.lock().unwrap(), vec![41, 42]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![41, 42]]);
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn native_on_is_a_noop_when_the_stack_matches() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[41, 42]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    /// A server stack extending *above* the submission is left standing:
    /// the submitted PRs are already its bottom prefix, and rebuilding
    /// would evict the upper PRs from merge-time retargeting.
    #[tokio::test]
    async fn native_on_leaves_a_superset_stack_standing() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[41, 42, 43]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    /// The eviction warning names each open PR that is dissolved along
    /// with a foreign stack but not restacked — once, however many stacks
    /// carried it.
    #[test]
    fn evicted_pr_numbers_reports_lost_members_once() {
        let stacks = vec![
            ForgeStack {
                number: 7,
                open_pr_numbers: vec![41, 99],
            },
            ForgeStack {
                number: 8,
                open_pr_numbers: vec![42, 99],
            },
        ];
        assert_eq!(evicted_pr_numbers(&stacks, &[41, 42]), vec![99]);
        assert!(evicted_pr_numbers(&stacks, &[41, 42, 99]).is_empty());
    }

    #[tokio::test]
    async fn native_on_appends_when_server_stack_is_a_bottom_prefix() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[41]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert_eq!(*forge.added_to_stacks.lock().unwrap(), vec![(7, vec![42])]);
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    /// A rejected append converges via dissolve-and-recreate instead of
    /// failing the submit.
    #[tokio::test]
    async fn add_conflict_falls_back_to_dissolve_and_recreate() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new()
            .with_existing_stack(7, &[41])
            .with_add_conflict();
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![7]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![41, 42]]);
    }

    /// A reordered stack cannot be converged by appending — dissolve and
    /// rebuild.
    #[tokio::test]
    async fn mismatched_stack_is_dissolved_and_recreated() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_existing_stack(7, &[42, 41]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![7]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![41, 42]]);
    }

    /// Desired PRs spread over several foreign stacks: all of them are
    /// dissolved before the new stack is created.
    #[tokio::test]
    async fn foreign_stacks_holding_desired_prs_are_dissolved() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new()
            .with_existing_stack(7, &[41, 99])
            .with_existing_stack(8, &[42]);
        run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap();

        let mut unstacked = forge.unstacked.lock().unwrap().clone();
        unstacked.sort_unstable();
        assert_eq!(unstacked, vec![7, 8]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![41, 42]]);
    }

    /// With `on`, an unavailable stacks API fails the submit — after the
    /// PRs themselves went through normally.
    #[tokio::test]
    async fn native_on_fails_the_submit_when_unavailable() {
        let mut plan = two_existing_pr_plan();
        // Make the PRs newly created so "submitted normally first" is
        // observable on the mock.
        for bp in &mut plan.bookmark_plans {
            bp.existing_pr = None;
            bp.needs_create = true;
            bp.needs_push = true;
        }
        let forge = MockForge::new().with_stacks_unavailable();
        let err = run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap_err();

        assert!(matches!(err, SubmitError::StacksUnavailable { .. }));
        assert_eq!(forge.created_prs.lock().unwrap().len(), 2);
        // The text placement still ran before the failure was reported.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
    }

    /// With `auto`, an unavailable stacks API is skipped silently and the
    /// requested placement carries on as if native stacks were never
    /// requested.
    #[tokio::test]
    async fn native_auto_skips_silently_when_unavailable() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_stacks_unavailable();
        run_plan(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Auto,
        )
        .await
        .unwrap();

        // Inactive: auto-comment behaves like comment.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    /// A failure that says nothing about availability must not stop the
    /// stack info from being written: a redundant comment self-heals on the
    /// next successful run, a skipped update leaves *stale* info standing.
    #[tokio::test]
    async fn native_auto_transient_error_still_writes_auto_comments() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_stacks_api_error();
        run_plan(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Auto,
        )
        .await
        .unwrap();

        // Unknown: auto-comment falls back to comment, not to ignore.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
    }

    /// The same transient error under `on` is a hard failure.
    #[tokio::test]
    async fn native_on_transient_error_fails_the_submit() {
        let plan = two_existing_pr_plan();
        let forge = MockForge::new().with_stacks_api_error();
        let err = run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitError::StackReconcileFailed { .. }));
    }

    /// With `on`, a reconcile failure fails the submit only *after* body
    /// syncs and the text placement ran, so everything the error's help
    /// text promises ("PRs were created/updated normally") has happened.
    /// In particular a requested `--sync-pr-content` body update must not
    /// be dropped by the failure.
    #[tokio::test]
    async fn native_on_reconcile_failure_still_syncs_bodies_and_writes_comments() {
        let mut plan = two_existing_pr_plan();
        plan.bookmark_plans[0].needs_body_sync = true;
        plan.bookmark_plans[0].body = Some("synced body".to_string());
        let forge = MockForge::new().with_stacks_api_error();
        let err = run_plan(&plan, &forge, StackPlacement::Comment, NativeStacks::On)
            .await
            .unwrap_err();

        assert!(matches!(err, SubmitError::StackReconcileFailed { .. }));
        assert_eq!(
            *forge.updated_bodies.lock().unwrap(),
            vec![(41, "synced body".to_string())]
        );
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
    }

    /// When the reconcile establishes a native stack, auto-comment retires
    /// stakk's own stack comment instead of updating it.
    #[tokio::test]
    async fn auto_comment_cleans_up_when_native_is_active() {
        let plan = two_existing_pr_plan();
        let stack_comment = Comment {
            id: 900,
            body: "<!--- STAKK_STACK: e30= --->\nold stack".to_string(),
        };
        let forge = MockForge::new().with_existing_comments(41, vec![stack_comment]);
        run_plan(&plan, &forge, StackPlacement::AutoComment, NativeStacks::On)
            .await
            .unwrap();

        // The native stack was registered, and the old comment retired.
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![41, 42]]);
        assert_eq!(*forge.deleted_comments.lock().unwrap(), vec![900]);
        assert!(forge.created_comments.lock().unwrap().is_empty());
        assert!(forge.updated_comments.lock().unwrap().is_empty());
    }

    /// A deferred body sync must still land when the placement resolves
    /// away from body mode (auto-body with a native stack in effect).
    #[tokio::test]
    async fn deferred_body_sync_applies_when_auto_body_resolves_to_cleanup() {
        let mut plan = two_existing_pr_plan();
        plan.bookmark_plans[0].needs_body_sync = true;
        plan.bookmark_plans[0].body = Some("synced body".to_string());
        let forge = MockForge::new();
        run_plan(&plan, &forge, StackPlacement::AutoBody, NativeStacks::On)
            .await
            .unwrap();

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(*updated_bodies, vec![(41, "synced body".to_string())]);
        // The synced body carries no fence — a native stack renders the
        // overview, stakk's text is retired.
        assert!(!updated_bodies[0].1.contains("STAKK_BODY_START"));
    }

    /// A single-PR submission takes the cleanup path even in body
    /// placement, so the body-splice phase never runs — the deferred body
    /// sync must be applied explicitly rather than dropped.
    #[tokio::test]
    async fn single_pr_body_placement_still_syncs_the_body() {
        let mut plan = two_existing_pr_plan();
        plan.bookmark_plans.truncate(1);
        plan.bookmark_plans[0].needs_body_sync = true;
        plan.bookmark_plans[0].body = Some("synced body".to_string());
        let forge = MockForge::new();
        run_plan(&plan, &forge, StackPlacement::Body, NativeStacks::Ignore)
            .await
            .unwrap();

        assert_eq!(
            *forge.updated_bodies.lock().unwrap(),
            vec![(41, "synced body".to_string())]
        );
    }
}
