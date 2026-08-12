//! Three-phase submission: analyze, plan, execute.
//!
//! Takes a change graph and forge implementation and submits bookmarks as
//! stacked pull requests, updating existing PRs idempotently.

mod trailers;
mod unwrap;

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
    #[error("native stacked pull requests are not enabled for this repository")]
    #[diagnostic(
        code(stakk::submit::stacks_unavailable),
        help(
            "your branches were pushed and PRs were created/updated normally — only the \
             server-side stack linkage was skipped. GitHub's stacked pull requests are a preview \
             feature that must be enabled per repository; set `--native-stacks auto` to use the \
             feature only where available, or `off` (env: STAKK_NATIVE_STACKS)"
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

/// Wrap a stack-API error, routing the not-enabled case to its dedicated
/// variant so miette renders the switch-placement guidance.
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
#[derive(Debug, Clone)]
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

/// Phase 2 output: the full submission plan.
#[derive(Debug)]
pub struct SubmissionPlan {
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
pub fn analyze_submission(
    target_bookmark: &str,
    change_graph: &ChangeGraph,
    default_branch: &str,
    selected_bookmarks: &HashSet<String>,
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
        let is_selected = seg
            .bookmark_names
            .iter()
            .any(|name| selected_bookmarks.contains(name));

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
    let known: HashSet<&str> = stack
        .segments
        .iter()
        .flat_map(|s| &s.bookmark_names)
        .map(String::as_str)
        .collect();
    let mut missing: Vec<String> = selected_bookmarks
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
/// determines whether to push, create, or update.
pub async fn create_submission_plan<F: Forge>(
    analysis: &SubmissionAnalysis,
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

    // Resolve whether a native server-side stack is in effect for this run.
    // The probe only runs for `auto`, and only when its answer is actually
    // consulted: by an auto placement, or by the reconcile step (which needs
    // more than one bookmark to form a stack).
    let native_state = match native {
        NativeStacks::Off => NativeState::Inactive,
        NativeStacks::On => NativeState::Active,
        NativeStacks::Auto => {
            let consulted = matches!(
                placement,
                StackPlacement::AutoComment | StackPlacement::AutoBody
            ) || plan.bookmark_plans.len() > 1;
            if consulted {
                match forge.supports_native_stacks().await {
                    Ok(true) => NativeState::Active,
                    Ok(false) => NativeState::Inactive,
                    Err(e) => {
                        pb.println(format!(
                            "  Warning: could not determine whether native stacked PRs are \
                             available: {e}. Stack reconciliation is skipped and auto placements \
                             behave like `ignore` this run."
                        ));
                        NativeState::Unknown
                    }
                }
            } else {
                NativeState::Unknown
            }
        }
    };
    let effective = resolve_placement(placement, native_state);

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

    // Step 4: When native stacks are in effect, converge the forge's
    // server-side stack with the submitted PRs. A single PR is not a stack,
    // so single-bookmark submissions skip the stack API entirely (a stale
    // server-side stack containing that PR is left alone).
    if native_state == NativeState::Active && stack_entries.len() > 1 {
        pb.set_message("Reconciling server-side stack...");
        let desired: Vec<u64> = stack_entries.iter().map(|e| e.pr_number).collect();
        reconcile_native_stack(forge, &desired, &pb).await?;
    }

    pb.finish_and_clear();

    Ok(SubmissionResult { stack_entries })
}

/// Whether a native server-side stack is in effect for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeState {
    /// The server-side stack will be reconciled.
    Active,
    /// Native stacks are off or definitively unavailable.
    Inactive,
    /// Availability could not be determined; avoid destructive decisions.
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

fn resolve_placement(placement: StackPlacement, native: NativeState) -> EffectivePlacement {
    match placement {
        StackPlacement::Comment => EffectivePlacement::Comment,
        StackPlacement::Body => EffectivePlacement::Body,
        StackPlacement::None => EffectivePlacement::Cleanup,
        StackPlacement::Ignore => EffectivePlacement::Ignore,
        StackPlacement::AutoComment => match native {
            NativeState::Active => EffectivePlacement::Cleanup,
            NativeState::Inactive => EffectivePlacement::Comment,
            NativeState::Unknown => EffectivePlacement::Ignore,
        },
        StackPlacement::AutoBody => match native {
            NativeState::Active => EffectivePlacement::Cleanup,
            NativeState::Inactive => EffectivePlacement::Body,
            NativeState::Unknown => EffectivePlacement::Ignore,
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
                // The preview API's add semantics are not fully specified;
                // if the server rejects the append, converge via the
                // universal dissolve-and-recreate path instead.
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
            // `unstack` removes unmerged PRs wholesale (the preview API has
            // no per-PR removal); merged leftovers cannot conflict with the
            // new stack.
            for stack in &stacks {
                forge.unstack(stack.number).await.map_err(wrap_stack_err)?;
            }
            let created = forge.create_stack(desired).await.map_err(wrap_stack_err)?;
            pb.println(format!(
                "  Recreated server-side stack #{} ({} PRs).",
                created.number,
                desired.len()
            ));
        }
    }

    Ok(())
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
    use crate::forge::ForgeStack;
    use crate::forge::PrState;
    use crate::forge::comment::build_comment_env;
    use crate::graph::types::BranchStack;
    use crate::graph::types::SegmentCommit;
    use crate::jj::JjError;

    // -- Shared operation log for ordering tests --

    type OpLog = Arc<Mutex<Vec<Op>>>;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
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
        existing_stacks: Vec<ForgeStack>,
        /// When set, `get_stacks_for_pr` fails with `StacksUnavailable`.
        stacks_unavailable: bool,
        /// When set, `add_to_stack` fails with `StackConflict`.
        add_conflicts: bool,
        /// What `supports_native_stacks` reports: `Some(bool)` is a
        /// definitive answer, `None` a transient failure.
        probe_supported: Option<bool>,
        /// Number of `supports_native_stacks` calls.
        probe_calls: Mutex<usize>,
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
                add_conflicts: false,
                probe_supported: Some(true),
                probe_calls: Mutex::new(0),
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

        fn with_add_conflict(mut self) -> Self {
            self.add_conflicts = true;
            self
        }

        fn with_probe_unsupported(mut self) -> Self {
            self.probe_supported = Some(false);
            self
        }

        fn with_probe_error(mut self) -> Self {
            self.probe_supported = None;
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

        fn get_stacks_for_pr(
            &self,
            pr_number: u64,
        ) -> impl std::future::Future<Output = Result<Vec<ForgeStack>, ForgeError>> + Send {
            self.stack_lookups.lock().unwrap().push(pr_number);
            let result = if self.stacks_unavailable {
                Err(ForgeError::StacksUnavailable {
                    message: "not enabled".to_string(),
                    source: "test".to_string().into(),
                })
            } else {
                // Snapshot semantics: the reconcile queries before mutating,
                // so fixture stacks need not reflect later mutations.
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
            let mut counter = self.next_stack_number.lock().unwrap();
            let number = *counter;
            *counter += 1;
            drop(counter);
            let stack = ForgeStack {
                number,
                open_pr_numbers: pr_numbers.to_vec(),
            };
            self.created_stacks
                .lock()
                .unwrap()
                .push(pr_numbers.to_vec());
            async move { Ok(stack) }
        }

        fn add_to_stack(
            &self,
            stack_number: u64,
            pr_numbers: &[u64],
        ) -> impl std::future::Future<Output = Result<ForgeStack, ForgeError>> + Send {
            let result = if self.add_conflicts {
                Err(ForgeError::StackConflict {
                    message: "conflict".to_string(),
                    source: "test".to_string().into(),
                })
            } else {
                self.added_to_stacks
                    .lock()
                    .unwrap()
                    .push((stack_number, pr_numbers.to_vec()));
                Ok(ForgeStack {
                    number: stack_number,
                    open_pr_numbers: pr_numbers.to_vec(),
                })
            };
            async move { result }
        }

        fn unstack(
            &self,
            stack_number: u64,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.unstacked.lock().unwrap().push(stack_number);
            async { Ok(()) }
        }

        fn supports_native_stacks(
            &self,
        ) -> impl std::future::Future<Output = Result<bool, ForgeError>> + Send {
            *self.probe_calls.lock().unwrap() += 1;
            let result = match self.probe_supported {
                Some(supported) => Ok(supported),
                None => Err(ForgeError::Api {
                    message: "probe failed".to_string(),
                    source: "test".to_string().into(),
                }),
            };
            async move { result }
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
            // Only handle push commands.
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

        let all = HashSet::from(["feat-a".to_string()]);
        let result = analyze_submission("feat-a", &graph, "main", &all).unwrap();
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

        let all = HashSet::from([
            "feat-a".to_string(),
            "feat-b".to_string(),
            "feat-c".to_string(),
        ]);
        let result = analyze_submission("feat-b", &graph, "main", &all).unwrap();
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

        let all = HashSet::from(["feat-a".to_string(), "feat-b".to_string()]);
        let result = analyze_submission("feat-b", &graph, "main", &all).unwrap();
        assert_eq!(result.segments.len(), 2);
    }

    #[test]
    fn analyze_bookmark_not_found() {
        let seg = make_segment(&["feat-a"], "ch_a", "feature a");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg],
        }]);

        let all = HashSet::from(["nonexistent".to_string()]);
        let result = analyze_submission("nonexistent", &graph, "main", &all);
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

        let all = HashSet::from(["beta".to_string(), "gamma".to_string()]);
        let result = analyze_submission("gamma", &graph, "main", &all).unwrap();
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
        let result = analyze_submission("feat-c", &graph, "main", &selected).unwrap();
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
        let result = analyze_submission("feat-c", &graph, "main", &selected).unwrap();
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
        let result = analyze_submission("feat-b", &graph, "main", &selected);
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
        let result = analyze_submission("feat-a", &graph, "main", &selected);
        match result {
            Err(SubmitError::SelectedBookmarksExcluded { missing, immutable }) => {
                assert_eq!(missing, vec!["vanished"]);
                assert!(immutable.is_empty());
            }
            other => panic!("expected SelectedBookmarksExcluded, got {other:?}"),
        }
    }

    /// The explicit `--bookmark <name>` path (selected == {target}) can never
    /// trigger the excluded-bookmarks guard.
    #[test]
    fn analyze_explicit_single_bookmark_never_triggers_guard() {
        let seg_a = make_segment(&["feat-a"], "ch_a", "feature a");
        let seg_b = make_segment(&["feat-b"], "ch_b", "feature b");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg_a, seg_b],
        }]);

        let selected = HashSet::from(["feat-b".to_string()]);
        let result = analyze_submission("feat-b", &graph, "main", &selected).unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].bookmark_names, vec!["feat-b"]);
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
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(output.contains("sync PR #42 title from commits"));
        assert!(output.contains("sync PR #42 body from commits"));
        assert!(!output.contains("up to date"));
    }

    // -----------------------------------------------------------------------
    // Phase 3 tests
    // -----------------------------------------------------------------------

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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
        )
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
        )
        .await
        .unwrap();

        assert!(forge.deleted_comments.lock().unwrap().is_empty());
        assert!(forge.listed_comments.lock().unwrap().is_empty());
    }

    // -- auto placements & native_stacks auto --

    async fn run_with(
        plan: &SubmissionPlan,
        forge: &MockForge,
        placement: StackPlacement,
        native: NativeStacks,
    ) -> SubmissionResult {
        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let env = test_comment_env();
        execute_submission_plan(plan, &jj, forge, &env, placement, native)
            .await
            .unwrap()
    }

    /// Two-bookmark all-create plan shared by the auto tests.
    fn two_create_plan() -> SubmissionPlan {
        SubmissionPlan {
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
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        }
    }

    #[tokio::test]
    async fn execute_auto_comment_native_off_writes_comments_without_probe() {
        let plan = two_create_plan();
        let forge = MockForge::new();

        run_with(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Off,
        )
        .await;

        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert_eq!(*forge.probe_calls.lock().unwrap(), 0);
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_auto_comment_native_auto_supported_cleans_up_and_reconciles() {
        let plan = two_create_plan();
        let forge = MockForge::new(); // probe: supported by default

        run_with(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Auto,
        )
        .await;

        // Native in effect: no comments, stack reconciled.
        assert!(forge.created_comments.lock().unwrap().is_empty());
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![100, 101]]);
        assert_eq!(*forge.probe_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn execute_auto_comment_native_auto_unsupported_writes_comments() {
        let plan = two_create_plan();
        let forge = MockForge::new().with_probe_unsupported();

        run_with(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Auto,
        )
        .await;

        // Fallback rendering: comments written, no stack API traffic beyond
        // the probe.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert_eq!(*forge.probe_calls.lock().unwrap(), 1);
        assert!(forge.stack_lookups.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_auto_comment_probe_error_behaves_like_ignore() {
        // PR #50 carries a stale stack comment; an indeterminate probe must
        // neither write nor remove anything, and must not fail the submit.
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
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };
        let forge = MockForge::new().with_probe_error().with_existing_comments(
            50,
            vec![Comment {
                id: 999,
                body: "<!--- STAKK_STACK: e30= --->\nold stack comment".to_string(),
            }],
        );

        let result = run_with(
            &plan,
            &forge,
            StackPlacement::AutoComment,
            NativeStacks::Auto,
        )
        .await;

        assert_eq!(result.stack_entries.len(), 2);
        assert!(forge.created_comments.lock().unwrap().is_empty());
        assert!(forge.deleted_comments.lock().unwrap().is_empty());
        assert!(forge.updated_bodies.lock().unwrap().is_empty());
        assert!(forge.listed_comments.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_auto_body_native_off_splices_fence() {
        let plan = two_create_plan();
        let forge = MockForge::new();

        run_with(&plan, &forge, StackPlacement::AutoBody, NativeStacks::Off).await;

        // Resolves to body mode: fences spliced into both PR bodies.
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 2);
        assert!(updated_bodies[0].1.contains("STAKK_BODY_START"));
        assert!(forge.created_comments.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_auto_body_native_on_syncs_body_in_loop() {
        // Native in effect resolves auto-body to cleanup, so a pending body
        // sync must not be deferred to the (never-running) splice phase.
        let mut plan = two_create_plan();
        plan.bookmark_plans[0].existing_pr = Some(make_pr(50, "feat-a", "main"));
        plan.bookmark_plans[0].needs_create = false;
        plan.bookmark_plans[0].needs_body_sync = true;
        plan.bookmark_plans[0].body = Some("new body".to_string());
        let forge = MockForge::new();

        run_with(&plan, &forge, StackPlacement::AutoBody, NativeStacks::On).await;

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(*updated_bodies, vec![(50, "new body".to_string())]);
    }

    #[tokio::test]
    async fn execute_native_auto_unsupported_skips_reconcile() {
        let plan = two_create_plan();
        let forge = MockForge::new().with_probe_unsupported();

        run_with(&plan, &forge, StackPlacement::Comment, NativeStacks::Auto).await;

        // Fixed placement unaffected; reconcile silently skipped.
        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert!(forge.stack_lookups.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_auto_supported_reconciles() {
        let plan = two_create_plan();
        let forge = MockForge::new();

        run_with(&plan, &forge, StackPlacement::Comment, NativeStacks::Auto).await;

        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![100, 101]]);
    }

    #[tokio::test]
    async fn execute_native_auto_single_bookmark_fixed_placement_skips_probe() {
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
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        };
        let forge = MockForge::new();

        run_with(&plan, &forge, StackPlacement::Comment, NativeStacks::Auto).await;

        // The probe's answer would never be consulted — no wasted API call.
        assert_eq!(*forge.probe_calls.lock().unwrap(), 0);
    }

    // -- native stacks --

    /// A bookmark plan that pushes and either reuses `existing` or creates.
    fn make_native_bookmark_plan(
        name: &str,
        base: &str,
        existing: Option<PullRequest>,
    ) -> BookmarkPlan {
        BookmarkPlan {
            bookmark_name: name.to_string(),
            base: base.to_string(),
            title: format!("title {name}"),
            body: None,
            needs_create: existing.is_none(),
            existing_pr: existing,
            needs_push: true,
            needs_base_update: false,
            needs_title_sync: false,
            needs_body_sync: false,
        }
    }

    fn make_native_plan(bookmark_plans: Vec<BookmarkPlan>) -> SubmissionPlan {
        SubmissionPlan {
            bookmark_plans,
            remote: "origin".to_string(),
            pr_mode: PrMode::Regular,
            default_branch: "main".to_string(),
        }
    }

    /// Run with native stacks on and `none` placement (the closest analog
    /// to the typical native configuration).
    async fn run_native(plan: &SubmissionPlan, forge: &MockForge) -> SubmissionResult {
        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let env = test_comment_env();
        execute_submission_plan(
            plan,
            &jj,
            forge,
            &env,
            StackPlacement::None,
            NativeStacks::On,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn execute_native_on_is_independent_of_comment_placement() {
        // native_stacks is orthogonal to stack placement: with comment
        // placement, both the stack comments and the server-side stack are
        // written.
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", None),
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new();
        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let env = test_comment_env();

        execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::Comment,
            NativeStacks::On,
        )
        .await
        .unwrap();

        assert_eq!(forge.created_comments.lock().unwrap().len(), 2);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![100, 101]]);
    }

    #[tokio::test]
    async fn execute_native_creates_stack_when_none_exists() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", None),
            make_native_bookmark_plan("feat-b", "feat-a", None),
            make_native_bookmark_plan("feat-c", "feat-b", None),
        ]);
        let forge = MockForge::new();

        let result = run_native(&plan, &forge).await;

        assert_eq!(result.stack_entries.len(), 3);
        // Mock-created PRs number from 100; the stack is created from the
        // submitted PRs bottom-to-top.
        let created_stacks = forge.created_stacks.lock().unwrap();
        assert_eq!(*created_stacks, vec![vec![100, 101, 102]]);
        // Every desired PR is queried for existing stack membership.
        let lookups = forge.stack_lookups.lock().unwrap();
        assert_eq!(*lookups, vec![100, 101, 102]);
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
        assert!(forge.created_comments.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_noop_when_stack_matches() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main"))),
            make_native_bookmark_plan("feat-b", "feat-a", Some(make_pr(51, "feat-b", "feat-a"))),
        ]);
        let forge = MockForge::new().with_existing_stack(500, &[50, 51]);

        run_native(&plan, &forge).await;

        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_adds_suffix_when_existing_stack_is_prefix() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main"))),
            make_native_bookmark_plan("feat-b", "feat-a", Some(make_pr(51, "feat-b", "feat-a"))),
            make_native_bookmark_plan("feat-c", "feat-b", None),
        ]);
        let forge = MockForge::new().with_existing_stack(500, &[50, 51]);

        run_native(&plan, &forge).await;

        // The new PR (100) is appended on top of the existing stack.
        let added = forge.added_to_stacks.lock().unwrap();
        assert_eq!(*added, vec![(500, vec![100])]);
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_recreates_stack_on_membership_mismatch() {
        // Server has the same PRs in the opposite order (a stack reorder).
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main"))),
            make_native_bookmark_plan("feat-b", "feat-a", Some(make_pr(51, "feat-b", "feat-a"))),
        ]);
        let forge = MockForge::new().with_existing_stack(500, &[51, 50]);

        run_native(&plan, &forge).await;

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![500]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![50, 51]]);
        assert!(forge.added_to_stacks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_recreates_stack_when_multiple_stacks_found() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main"))),
            make_native_bookmark_plan("feat-b", "feat-a", Some(make_pr(51, "feat-b", "feat-a"))),
        ]);
        let forge = MockForge::new()
            .with_existing_stack(500, &[50])
            .with_existing_stack(501, &[51]);

        run_native(&plan, &forge).await;

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![500, 501]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![50, 51]]);
    }

    #[tokio::test]
    async fn reconcile_recreates_stack_with_foreign_member() {
        // The existing stack holds a PR that is not part of the submission;
        // desired is not a prefix-extension of it, so it is rebuilt.
        let forge = MockForge::new().with_existing_stack(500, &[999, 50, 51]);
        let pb = indicatif::ProgressBar::hidden();

        reconcile_native_stack(&forge, &[50, 51], &pb)
            .await
            .unwrap();

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![500]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![50, 51]]);
    }

    #[tokio::test]
    async fn execute_native_add_conflict_falls_back_to_recreate() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main"))),
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new()
            .with_existing_stack(500, &[50])
            .with_add_conflict();

        run_native(&plan, &forge).await;

        assert_eq!(*forge.unstacked.lock().unwrap(), vec![500]);
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![50, 100]]);
    }

    #[tokio::test]
    async fn execute_native_cleans_up_existing_artifacts() {
        use crate::forge::comment::splice_stack_into_body;

        // PR #50 carries both an old stack comment and a body fence from a
        // previous comment/body-mode submit.
        let body_with_fence = splice_stack_into_body("Original PR body", "old stack content");
        let old_comment_body = "<!--- STAKK_STACK: e30= --->\nold stack comment".to_string();

        let plan = make_native_plan(vec![
            make_native_bookmark_plan(
                "feat-a",
                "main",
                Some(make_pr_with_body(50, "feat-a", "main", &body_with_fence)),
            ),
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new()
            .with_existing_comments(
                50,
                vec![Comment {
                    id: 999,
                    body: old_comment_body,
                }],
            )
            .with_existing_stack(500, &[50, 100]);

        run_native(&plan, &forge).await;

        // Old stack comment deleted and body fence stripped, like `none`.
        assert_eq!(*forge.deleted_comments.lock().unwrap(), vec![999]);
        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(updated_bodies.len(), 1);
        assert!(!updated_bodies[0].1.contains("STAKK_BODY_START"));
        // The server-side stack already matches — no mutation.
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_native_writes_no_comments_or_bodies() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", None),
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new();

        run_native(&plan, &forge).await;

        assert!(forge.created_comments.lock().unwrap().is_empty());
        assert!(forge.updated_comments.lock().unwrap().is_empty());
        assert!(forge.updated_bodies.lock().unwrap().is_empty());
        // Freshly created PRs carry no artifacts — no lookups either.
        assert!(forge.listed_comments.lock().unwrap().is_empty());
        // But the stack is reconciled.
        assert_eq!(*forge.created_stacks.lock().unwrap(), vec![vec![100, 101]]);
    }

    #[tokio::test]
    async fn execute_native_single_bookmark_skips_stack_api() {
        let plan = make_native_plan(vec![make_native_bookmark_plan(
            "feat-a",
            "main",
            Some(make_pr(50, "feat-a", "main")),
        )]);
        let forge = MockForge::new().with_existing_stack(500, &[50, 51]);

        run_native(&plan, &forge).await;

        // A single PR is not a stack: no stack API traffic at all, even
        // though the server holds a stale stack containing the PR.
        assert!(forge.stack_lookups.lock().unwrap().is_empty());
        assert!(forge.created_stacks.lock().unwrap().is_empty());
        assert!(forge.unstacked.lock().unwrap().is_empty());
        // Artifact cleanup still runs on the pre-existing PR.
        assert_eq!(*forge.listed_comments.lock().unwrap(), vec![50]);
    }

    #[tokio::test]
    async fn execute_native_stacks_unavailable_surfaces_diagnostic() {
        let plan = make_native_plan(vec![
            make_native_bookmark_plan("feat-a", "main", None),
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new().with_stacks_unavailable();
        let (runner, push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let env = test_comment_env();

        let result = execute_submission_plan(
            &plan,
            &jj,
            &forge,
            &env,
            StackPlacement::None,
            NativeStacks::On,
        )
        .await;

        assert!(matches!(result, Err(SubmitError::StacksUnavailable { .. })));
        // The submission itself completed before the reconcile failed.
        assert_eq!(forge.created_prs.lock().unwrap().len(), 2);
        assert_eq!(push_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn execute_native_body_sync_not_deferred() {
        // Unlike body mode, native placement has no fence-splicing phase to
        // fold the body sync into — it must happen in the per-bookmark loop.
        let mut bp =
            make_native_bookmark_plan("feat-a", "main", Some(make_pr(50, "feat-a", "main")));
        bp.needs_body_sync = true;
        bp.body = Some("new body".to_string());
        let plan = make_native_plan(vec![
            bp,
            make_native_bookmark_plan("feat-b", "feat-a", None),
        ]);
        let forge = MockForge::new().with_existing_stack(500, &[50, 100]);

        run_native(&plan, &forge).await;

        let updated_bodies = forge.updated_bodies.lock().unwrap();
        assert_eq!(*updated_bodies, vec![(50, "new body".to_string())]);
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
            NativeStacks::Off,
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
}
