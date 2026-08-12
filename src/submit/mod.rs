//! Three-phase submission: analyze, plan, execute.
//!
//! Takes a change graph and forge implementation and submits bookmarks as
//! stacked pull requests, updating existing PRs idempotently.

mod trailers;
mod unwrap;

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

use crate::cli::submit::PrMode;
use crate::cli::submit::SyncPrContent;
use crate::cli::submit::TrailerHandling;
use crate::forge::CreatePrParams;
use crate::forge::Forge;
use crate::forge::ForgeError;
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
use crate::graph::types::ChangeGraph;
use crate::graph::types::SegmentCommit;
use crate::jj::Jj;
use crate::jj::JjError;
use crate::jj::runner::JjRunner;
use crate::submit::trailers::split_trailers;
use crate::submit::unwrap::unwrap_markdown;

/// Errors from the submission pipeline.
#[derive(Debug, Error, Diagnostic)]
pub enum SubmitError {
    /// Target bookmark was not found in any stack.
    #[error("bookmark '{bookmark}' not found in any stack")]
    #[diagnostic(
        code(stakk::submit::bookmark_not_found),
        help("run `stakk` with no arguments to see available stacks")
    )]
    BookmarkNotFound { bookmark: String },

    /// Selected bookmarks were never consumed by any segment in the target
    /// stack — typically because their commits are immutable in jj, so the
    /// bookmarks revset excluded them from the change graph.
    #[error(
        "selected bookmark(s) not in the submission stack: {} ({} on immutable commit(s))",
        missing.join(", "),
        if immutable.is_empty() { "none".to_string() } else { immutable.join(", ") }
    )]
    #[diagnostic(
        code(stakk::submit::selected_bookmarks_excluded),
        help(
            "the bookmark(s) exist and point at the right commits, but the commits are immutable \
             in jj, so the default --bookmarks-revset (mine() ~ trunk() ~ immutable()) excludes \
             them from the graph — usually caused by stale untracked remote bookmarks pinning the \
             commits. Fix the cause with `jj bookmark forget --include-remotes 'glob:<pattern>'`, \
             or include immutable commits for one run with `--bookmarks-revset 'mine() ~ \
             trunk()'` (env: STAKK_BOOKMARKS_REVSET)"
        )
    )]
    SelectedBookmarksExcluded {
        /// Selected names not consumed by any segment (sorted).
        missing: Vec<String>,
        /// Subset of `missing` found on immutable commits in the stack
        /// (sorted).
        immutable: Vec<String>,
    },

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
    /// selection's `is_new` assignments; empty for the positional
    /// `stakk submit <bookmark>` path where every bookmark already exists.
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

/// Find the segments relevant to submitting the target bookmark.
///
/// Locates the stack containing `target_bookmark` in the change graph and
/// returns all segments from trunk to the target (inclusive).
///
/// `selected_bookmarks` controls which segment boundaries survive as their
/// own PR: segments whose bookmarks are all unselected fold their commits
/// into the next selected segment. `None` selects every boundary — the
/// explicit `stakk submit <bookmark>` path, where the whole ancestor chain
/// is submitted as stacked PRs.
pub fn analyze_submission(
    target_bookmark: &str,
    change_graph: &ChangeGraph,
    default_branch: &str,
    selected_bookmarks: Option<&HashSet<String>>,
) -> Result<SubmissionAnalysis, SubmitError> {
    let stack = change_graph
        .stacks
        .iter()
        .find(|s| {
            s.segments
                .iter()
                .any(|seg| seg.bookmark_names.contains(&target_bookmark.to_string()))
        })
        .ok_or_else(|| SubmitError::BookmarkNotFound {
            bookmark: target_bookmark.to_string(),
        })?;

    let target_index = stack
        .segments
        .iter()
        .position(|seg| seg.bookmark_names.contains(&target_bookmark.to_string()))
        .expect("bookmark was found in stack above");

    let mut segments = Vec::new();
    let mut accumulated_commits: Vec<SegmentCommit> = Vec::new();

    for seg in &stack.segments[..=target_index] {
        let is_selected = selected_bookmarks.is_none_or(|selected| {
            seg.bookmark_names
                .iter()
                .any(|name| selected.contains(name))
        });

        if is_selected {
            let mut commits = seg.commits.clone();
            commits.append(&mut accumulated_commits);
            segments.push(BookmarkSegment {
                bookmark_names: seg.bookmark_names.clone(),
                change_id: seg.change_id.clone(),
                commits,
            });
        } else {
            accumulated_commits.extend(seg.commits.iter().cloned());
        }
    }

    // Every selected bookmark must be a segment boundary somewhere in the
    // stack. A selected name matching no segment means the graph excluded it
    // (its commit is in the stack, but the bookmarks revset filtered the
    // bookmark — typically because the commit is immutable). Silently folding
    // it away would submit fewer PRs than the user selected. Selected names
    // above the target are fine: they're boundaries, just not part of this
    // submission.
    if let Some(selected) = selected_bookmarks {
        let known: HashSet<&str> = stack
            .segments
            .iter()
            .flat_map(|s| &s.bookmark_names)
            .map(String::as_str)
            .collect();
        let mut missing: Vec<String> = selected
            .iter()
            .filter(|name| !known.contains(name.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            let immutable: Vec<String> = missing
                .iter()
                .filter(|name| {
                    stack
                        .segments
                        .iter()
                        .flat_map(|s| &s.commits)
                        .any(|c| c.is_immutable && c.local_bookmark_names.contains(name))
                })
                .cloned()
                .collect();
            return Err(SubmitError::SelectedBookmarksExcluded { missing, immutable });
        }
    }

    Ok(SubmissionAnalysis {
        segments,
        default_branch: default_branch.to_string(),
    })
}

/// Build a submission analysis directly from an explicit selection.
///
/// `path` is the full trunk-to-tip commit chain of the selected stack and
/// `assignments` (trunk-to-leaf) name the commits that become segment
/// boundaries. Commits between boundaries belong to the boundary above them
/// — the same fold semantics as `analyze_submission` with a selected subset.
/// Commits above the last boundary are not part of the submission.
///
/// Unlike `analyze_submission`, no bookmark lookup happens: boundaries are
/// matched by change ID, so bookmarks that do not exist yet (`is_new`
/// assignments) work without creating them first or rebuilding the graph.
/// That keeps `--dry-run` free of side effects; the execute phase performs
/// the actual `jj bookmark create` calls.
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
/// keeps only the assigned name — the name the user actually chose — rather
/// than all names like `analyze_submission` does. With several consecutive
/// folded segments the folded commits are ordered strictly newest-first,
/// per `BookmarkSegment::commits`' convention.
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
/// the local bookmarks the execute phase must create first (empty for the
/// positional path, where every bookmark already exists); taking them here
/// keeps the returned plan complete at construction.
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
) -> Result<SubmissionResult, SubmitError> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    let effective = resolve_placement(placement);

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

        // Body sync: skip when body-mode stacking is active — the
        // body-mode stack phase will splice the fence onto bp.body,
        // combining both updates into a single API call.
        if bp.needs_body_sync
            && effective != EffectivePlacement::Body
            && let Some(pr) = &bp.existing_pr
        {
            let new_body = bp.body.as_deref().unwrap_or("");
            pb.set_message(format!("Syncing PR #{} body...", pr.number));
            forge
                .update_pr_body(pr.number, new_body)
                .await
                .map_err(|source| SubmitError::BodySyncFailed {
                    pr_number: pr.number,
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
        }

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

                                // Migration: if no existing fenced section was found,
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

    Ok(SubmissionResult { stack_entries })
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

fn resolve_placement(placement: StackPlacement) -> EffectivePlacement {
    match placement {
        StackPlacement::Comment => EffectivePlacement::Comment,
        StackPlacement::Body => EffectivePlacement::Body,
        StackPlacement::None => EffectivePlacement::Cleanup,
        StackPlacement::Ignore => EffectivePlacement::Ignore,
    }
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
    use crate::forge::PrState;
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
                short_change_id: change_id[..4.min(change_id.len())].to_string(),
            }],
        }
    }

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: std::collections::HashSet::new(),
            stack_roots: std::collections::HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: std::collections::HashSet::new(),
            excluded_bookmark_count: 0,
            stacks,
        }
    }

    fn make_pr(number: u64, head: &str, base: &str) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://github.com/test/repo/pull/{number}"),
            title: format!("PR for {head}"),
            head_ref: head.to_string(),
            base_ref: base.to_string(),
            state: PrState::Open,
            body: None,
        }
    }

    fn make_pr_with_body(number: u64, head: &str, base: &str, body: &str) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://github.com/test/repo/pull/{number}"),
            title: format!("PR for {head}"),
            head_ref: head.to_string(),
            base_ref: base.to_string(),
            state: PrState::Open,
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
    }

    impl Forge for MockForge {
        async fn get_authenticated_user(&self) -> Result<String, ForgeError> {
            Ok("test-user".to_string())
        }

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
                head_ref: params.head.clone(),
                base_ref: params.base.clone(),
                state: PrState::Open,
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
    }

    // -- Mock JjRunner --

    type PushLog = Arc<Mutex<Vec<(String, String)>>>;

    struct MockJjRunner {
        push_calls: PushLog,
        ops: Option<OpLog>,
    }

    impl MockJjRunner {
        fn new() -> (Self, PushLog) {
            let calls: PushLog = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    push_calls: Arc::clone(&calls),
                    ops: None,
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
                },
                calls,
            )
        }
    }

    impl crate::jj::runner::JjRunner for MockJjRunner {
        fn run_jj(
            &self,
            args: &[&str],
        ) -> impl std::future::Future<Output = Result<String, JjError>> + Send {
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
            async { Ok(String::new()) }
        }
    }

    // -----------------------------------------------------------------------
    // Phase 1 tests
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_single_bookmark() {
        let seg = make_segment(&["feat-a"], "ch_a", "add feature a");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg],
        }]);

        let result = analyze_submission("feat-a", &graph, "main", None).unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-a"]);

        assert_eq!(result.default_branch, "main");
    }

    #[test]
    fn analyze_middle_of_stack() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let seg_c = make_segment(&["feat-c"], "ch_c", "feature c");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b, seg_c],
        }]);

        let result = analyze_submission("feat-b", &graph, "main", None).unwrap();
        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(result.segments[1].bookmark_names, vec!["feat-b"]);
    }

    #[test]
    fn analyze_leaf_of_stack() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b],
        }]);

        let result = analyze_submission("feat-b", &graph, "main", None).unwrap();
        assert_eq!(result.segments.len(), 2);
    }

    #[test]
    fn analyze_bookmark_not_found() {
        let seg = make_segment(&["feat-a"], "ch_a", "feature a");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg],
        }]);

        let result = analyze_submission("nonexistent", &graph, "main", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention the bookmark name: {err}"
        );
    }

    #[test]
    fn analyze_multiple_stacks_finds_correct_one() {
        let stack1 = BranchStack {
            segments: vec![make_segment(&["alpha"], "ch_alpha", "alpha")],
        };
        let stack2 = BranchStack {
            segments: vec![
                make_segment(&["beta"], "ch_beta", "beta"),
                make_segment(&["gamma"], "ch_gamma", "gamma"),
            ],
        };
        let graph = make_graph(vec![stack1, stack2]);

        let result = analyze_submission("gamma", &graph, "main", None).unwrap();
        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.segments[0].bookmark_names, vec!["beta"]);
        assert_eq!(result.segments[1].bookmark_names, vec!["gamma"]);
    }

    #[test]
    fn analyze_filters_unselected_bookmarks() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let seg_c = make_segment(&["feat-c"], "ch_c", "feature c");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b, seg_c],
        }]);

        // Only select the leaf — intermediate bookmarks should be excluded,
        // but their commits fold into the next retained segment.
        let selected = HashSet::from(["feat-c".to_string()]);
        let result = analyze_submission("feat-c", &graph, "main", Some(&selected)).unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-c"]);
        assert_eq!(result.segments[0].commits.len(), 3); // C's own + B's + A's
    }

    #[test]
    fn analyze_filters_keeps_selected_subset() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let seg_c = make_segment(&["feat-c"], "ch_c", "feature c");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b, seg_c],
        }]);

        // Select first and last — middle should be excluded,
        // and middle's commits fold into the next retained segment.
        let selected = HashSet::from(["feat-a".to_string(), "feat-c".to_string()]);
        let result = analyze_submission("feat-c", &graph, "main", Some(&selected)).unwrap();
        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(result.segments[0].commits.len(), 1); // A's own only
        assert_eq!(result.segments[1].bookmark_names, vec!["feat-c"]);
        assert_eq!(result.segments[1].commits.len(), 2); // C's own + B's inherited
    }

    /// A selected bookmark that never becomes a segment boundary must error
    /// loudly, with immutable-commit diagnosis from the graph data.
    #[test]
    fn analyze_errors_when_selected_bookmark_excluded() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        // feat-a's segment contains an immutable mid-segment commit carrying
        // the filtered-out bookmark "ghost".
        let mut seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        seg_b.commits[0].is_immutable = true;
        seg_b.commits[0].local_bookmark_names = vec!["ghost".to_string()];
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b],
        }]);

        let selected = HashSet::from(["feat-b".to_string(), "ghost".to_string()]);
        let result = analyze_submission("feat-b", &graph, "main", Some(&selected));
        match result {
            Err(SubmitError::SelectedBookmarksExcluded { missing, immutable }) => {
                assert_eq!(missing, vec!["ghost"]);
                assert_eq!(immutable, vec!["ghost"]);
            }
            other => panic!("expected SelectedBookmarksExcluded, got {other:?}"),
        }
    }

    /// A missing selected name on no known commit still errors, but without
    /// the immutable diagnosis.
    #[test]
    fn analyze_excluded_error_distinguishes_non_immutable_missing() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a],
        }]);

        let selected = HashSet::from(["feat-a".to_string(), "vanished".to_string()]);
        let result = analyze_submission("feat-a", &graph, "main", Some(&selected));
        match result {
            Err(SubmitError::SelectedBookmarksExcluded { missing, immutable }) => {
                assert_eq!(missing, vec!["vanished"]);
                assert!(immutable.is_empty());
            }
            other => panic!("expected SelectedBookmarksExcluded, got {other:?}"),
        }
    }

    /// An interactive selection of only the target (selected == {target})
    /// can never trigger the excluded-bookmarks guard.
    #[test]
    fn analyze_selected_single_bookmark_never_triggers_guard() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b],
        }]);

        let selected = HashSet::from(["feat-b".to_string()]);
        let result = analyze_submission("feat-b", &graph, "main", Some(&selected)).unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-b"]);
    }

    /// Regression test for <https://github.com/glennib/stakk/issues/184>:
    /// `stakk submit <leaf>` (no interactive selection) must submit the leaf
    /// and every bookmarked ancestor as separate stacked segments, not fold
    /// the ancestors into one cumulative leaf PR.
    #[test]
    fn analyze_explicit_target_submits_all_ancestors() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let seg_c = make_segment(&["feat-c"], "ch_c", "feature c");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b, seg_c],
        }]);

        let result = analyze_submission("feat-c", &graph, "main", None).unwrap();
        assert_eq!(result.segments.len(), 3);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-a"]);
        assert_eq!(result.segments[1].bookmark_names, vec!["feat-b"]);
        assert_eq!(result.segments[2].bookmark_names, vec!["feat-c"]);
        // No folding: each segment keeps exactly its own commit.
        for seg in &result.segments {
            assert_eq!(seg.commits.len(), 1);
        }
    }

    // -----------------------------------------------------------------------
    // analysis_from_selection tests
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

    /// Leaf-only selection matches `analyze_submission` with the same
    /// selected set: the folded ancestor joins the leaf PR.
    #[test]
    fn from_selection_parity_leaf_only() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],
        };
        let path = path_of(&stack);
        let graph = make_graph(vec![stack]);

        let selected = HashSet::from(["feat-b".to_string()]);
        let reference = analyze_submission("feat-b", &graph, "main", Some(&selected)).unwrap();

        let direct =
            analysis_from_selection(&path, &[make_assignment("ch_b", "feat-b", false)], "main")
                .unwrap();

        assert_eq!(direct, reference);
    }

    /// Keeping a subset folds the unkept middle segment into the one above,
    /// exactly like `analyze_submission`.
    #[test]
    fn from_selection_parity_subset() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
                make_segment(&["feat-c"], "ch_c", "feature c"),
            ],
        };
        let path = path_of(&stack);
        let graph = make_graph(vec![stack]);

        let selected = HashSet::from(["feat-a".to_string(), "feat-c".to_string()]);
        let reference = analyze_submission("feat-c", &graph, "main", Some(&selected)).unwrap();

        let direct = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_a", "feat-a", false),
                make_assignment("ch_c", "feat-c", false),
            ],
            "main",
        )
        .unwrap();

        assert_eq!(direct, reference);
    }

    /// Selecting every boundary matches the positional no-folding path.
    #[test]
    fn from_selection_parity_all_boundaries() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", "feature a"),
                make_segment(&["feat-b"], "ch_b", "feature b"),
            ],
        };
        let path = path_of(&stack);
        let graph = make_graph(vec![stack]);

        let reference = analyze_submission("feat-b", &graph, "main", None).unwrap();

        let direct = analysis_from_selection(
            &path,
            &[
                make_assignment("ch_a", "feat-a", false),
                make_assignment("ch_b", "feat-b", false),
            ],
            "main",
        )
        .unwrap();

        assert_eq!(direct, reference);
    }

    /// A new bookmark on a mid-segment commit splits the segment — possible
    /// without the bookmark existing anywhere, unlike `analyze_submission`.
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

    /// Commits above the topmost assignment are not part of the submission
    /// (parity with `analyze_submission`'s trunk..=target slice).
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
    /// accumulate. (`analyze_submission` interleaves folded segments in
    /// trunk-to-leaf order here — a quirk this constructor does not copy;
    /// same commit set, stated convention for the order.)
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
        let graph = make_graph(vec![stack]);

        let direct =
            analysis_from_selection(&path, &[make_assignment("ch_d", "feat-d", false)], "main")
                .unwrap();

        assert_eq!(direct.segments.len(), 1);
        let ids: Vec<&str> = direct.segments[0]
            .commits
            .iter()
            .map(|c| c.change_id.as_str())
            .collect();
        assert_eq!(ids, vec!["ch_d", "ch_c", "ch_b", "ch_a"]);

        // Same commit set as the analyze path, ordering aside.
        let selected = HashSet::from(["feat-d".to_string()]);
        let reference = analyze_submission("feat-d", &graph, "main", Some(&selected)).unwrap();
        let mut reference_ids: Vec<String> = reference.segments[0]
            .commits
            .iter()
            .map(|c| c.change_id.clone())
            .collect();
        let mut direct_ids: Vec<String> =
            ids.iter().map(std::string::ToString::to_string).collect();
        reference_ids.sort_unstable();
        direct_ids.sort_unstable();
        assert_eq!(direct_ids, reference_ids);
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        let result = execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Body)
            .await
            .unwrap();

        // Body sync is skipped in the per-bookmark loop when body-mode is
        // active — the fence-splicing phase handles it in a single API call.
        // So there should be exactly 2 body updates (one per PR), not 4.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        // Both bodies should contain the commit text and the STAKK_BODY_START fence.
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Body)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Body)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Body)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        let result = execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Body)
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

        let result = execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::None)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::None)
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

        let result = execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Ignore)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Ignore)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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

        execute_submission_plan(&plan, &jj, &forge, &env, StackPlacement::Comment)
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
}
