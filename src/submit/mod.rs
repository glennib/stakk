//! Three-phase submission: analyze, plan, execute.
//!
//! Takes a change graph and forge implementation and submits bookmarks as
//! stacked pull requests, updating existing PRs idempotently.

mod unwrap;

use std::collections::HashSet;
use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

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

    /// A segment in the change graph has no bookmark name.
    #[error("segment has no bookmark name")]
    #[diagnostic(code(stakk::submit::segment_missing_bookmark))]
    SegmentMissingBookmark,

    /// Failed to look up an existing PR for a bookmark.
    #[error("failed to check for existing PR for '{bookmark}'")]
    #[diagnostic(code(stakk::submit::pr_lookup_failed))]
    PrLookupFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to push a bookmark to the remote.
    #[error("failed to push bookmark '{bookmark}'")]
    #[diagnostic(code(stakk::submit::push_failed))]
    PushFailed {
        bookmark: String,
        #[source]
        source: JjError,
    },

    /// Failed to update the base branch of an existing PR.
    #[error("failed to update PR base for '{bookmark}'")]
    #[diagnostic(code(stakk::submit::base_update_failed))]
    BaseUpdateFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to create a new PR.
    #[error("failed to create PR for '{bookmark}'")]
    #[diagnostic(code(stakk::submit::pr_create_failed))]
    PrCreateFailed {
        bookmark: String,
        #[source]
        source: ForgeError,
    },

    /// Failed to create or update a stack comment on a PR.
    #[error("failed to manage stack comment on PR #{pr_number}")]
    #[diagnostic(code(stakk::submit::comment_failed))]
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
    #[diagnostic(code(stakk::submit::body_update_failed))]
    BodyUpdateFailed {
        pr_number: u64,
        #[source]
        source: ForgeError,
    },
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
}

/// Phase 2 output: the full submission plan.
#[derive(Debug)]
pub struct SubmissionPlan {
    /// Per-bookmark plans, ordered trunk-to-leaf.
    pub bookmark_plans: Vec<BookmarkPlan>,
    /// The remote name to push to.
    pub remote: String,
    /// Whether to create PRs as drafts.
    pub draft: bool,
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
fn build_pr_body(commits: &[SegmentCommit]) -> Option<String> {
    if commits.is_empty() {
        return None;
    }

    let body = if commits.len() == 1 {
        // Single commit: strip the first line (title) and use the rest.
        let desc = commits[0].description.trim();
        let rest = desc.lines().skip(1).collect::<Vec<_>>().join("\n");
        rest.trim().to_string()
    } else {
        // Multiple commits: concatenate all descriptions.
        let parts: Vec<&str> = commits
            .iter()
            .map(|c| c.description.trim())
            .filter(|d: &&str| !d.is_empty())
            .collect();
        parts.join("\n\n---\n\n")
    };

    if body.is_empty() {
        None
    } else {
        Some(unwrap_markdown(&body))
    }
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
    draft: bool,
) -> Result<SubmissionPlan, SubmitError> {
    // Collect bookmark names for concurrent PR lookup.
    let bookmark_names: Vec<String> = analysis
        .segments
        .iter()
        .map(|seg| {
            seg.bookmark_names
                .first()
                .cloned()
                .ok_or(SubmitError::SegmentMissingBookmark)
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

        let body = build_pr_body(&segment.commits);

        bookmark_plans.push(BookmarkPlan {
            bookmark_name,
            base,
            title,
            body,
            existing_pr,
            needs_push: true,
            needs_create,
            needs_base_update,
        });
    }

    Ok(SubmissionPlan {
        bookmark_plans,
        remote: remote.to_string(),
        draft,
        default_branch: analysis.default_branch.clone(),
    })
}

// ---------------------------------------------------------------------------
// Phase 2: Display (for --dry-run)
// ---------------------------------------------------------------------------

impl fmt::Display for SubmissionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let draft_label = if self.draft { ", draft" } else { "" };
        writeln!(
            f,
            "Submission plan ({} bookmark(s), remote: {}{draft_label}):",
            self.bookmark_plans.len(),
            self.remote,
        )?;

        for bp in &self.bookmark_plans {
            writeln!(f, "  {} (base: {})", bp.bookmark_name, bp.base)?;
            if bp.needs_push {
                writeln!(f, "    - push bookmark to {}", self.remote)?;
            }
            if bp.needs_create {
                writeln!(f, "    - create PR: \"{}\"", bp.title)?;
            }
            if bp.needs_base_update
                && let Some(pr) = &bp.existing_pr
            {
                writeln!(
                    f,
                    "    - update PR #{} base: {} -> {}",
                    pr.number, pr.base_ref, bp.base,
                )?;
            }
            if !bp.needs_create
                && !bp.needs_base_update
                && let Some(pr) = &bp.existing_pr
            {
                writeln!(f, "    - PR #{} up to date", pr.number)?;
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

    let mut stack_entries = Vec::new();

    // Step 1: Push all bookmarks.
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
    }

    // Step 2a: Concurrently update bases for existing PRs that need it.
    pb.set_message("Updating PR bases...");
    let base_update_futures: Vec<_> = plan
        .bookmark_plans
        .iter()
        .filter(|bp| bp.needs_base_update)
        .filter_map(|bp| {
            bp.existing_pr
                .as_ref()
                .map(|pr| (bp.bookmark_name.clone(), pr.number, bp.base.clone()))
        })
        .map(|(name, number, base)| async move {
            forge.update_pr_base(number, &base).await.map_err(|source| {
                SubmitError::BaseUpdateFailed {
                    bookmark: name,
                    source,
                }
            })
        })
        .collect();
    let base_results = futures::future::join_all(base_update_futures).await;
    for result in base_results {
        result?;
    }

    // Step 2b: Create new PRs sequentially (base branch must exist first).
    for bp in &plan.bookmark_plans {
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
                    draft: plan.draft,
                })
                .await
                .map_err(|source| SubmitError::PrCreateFailed {
                    bookmark: bp.bookmark_name.clone(),
                    source,
                })?;
            pb.println(format!("  Created PR #{}: {}", pr.number, pr.html_url,));
            pr
        };

        stack_entries.push(StackEntry {
            bookmark_name: bp.bookmark_name.clone(),
            pr_url: pr.html_url.clone(),
            pr_number: pr.number,
        });
    }

    // Step 3: Concurrently create/update stack comments on all PRs.
    // For single-bookmark submissions, skip stack info entirely and just
    // clean up any stale stack artifacts from a previously larger stack.
    if stack_entries.len() > 1 {
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
                    is_draft: plan.draft && bp.needs_create,
                    position: i + 1,
                    is_current: false, // set per-PR below
                }
            })
            .collect();

        match placement {
            StackPlacement::Comment => {
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
                        let existing_body = plan.bookmark_plans[i]
                            .existing_pr
                            .as_ref()
                            .and_then(|pr| pr.body.clone());
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
            }
            StackPlacement::Body => {
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
                            let existing_body = if bp.needs_create {
                                // For newly created PRs, use the body we just
                                // submitted.
                                bp.body.clone().unwrap_or_default()
                            } else {
                                bp.existing_pr
                                    .as_ref()
                                    .and_then(|pr| pr.body.clone())
                                    .unwrap_or_default()
                            };
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
            }
        }
    } else if stack_entries.len() == 1 {
        // Single bookmark — not a stack. Clean up any stale stack artifacts
        // from when this PR was part of a larger stack.
        let entry = &stack_entries[0];
        let pr_number = entry.pr_number;
        let existing_body = plan.bookmark_plans[0]
            .existing_pr
            .as_ref()
            .and_then(|pr| pr.body.clone());

        // Clean up old stack comment (from either comment mode or pre-migration).
        let comments = forge
            .list_comments(pr_number)
            .await
            .map_err(|source| SubmitError::CommentFailed { pr_number, source })?;
        if let Some(old) = find_stack_comment(&comments)
            && let Err(e) = forge.delete_comment(old.id).await
        {
            pb.println(format!(
                "  Warning: failed to clean up old stack comment on PR #{pr_number}: {e}"
            ));
        }

        // Clean up old body fence (from body mode).
        if let Some(body) = &existing_body
            && find_stack_in_body(body).is_some()
        {
            let stripped = strip_stack_from_body(body);
            if let Err(e) = forge.update_pr_body(pr_number, &stripped).await {
                pb.println(format!(
                    "  Warning: failed to strip stack from PR #{pr_number} body: {e}"
                ));
            }
        }
    }

    pb.finish_and_clear();

    Ok(SubmissionResult { stack_entries })
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
                files: vec![],
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
        updated_bodies: Mutex<Vec<(u64, String)>>,
        deleted_comments: Mutex<Vec<u64>>,
        existing_comments: HashMap<u64, Vec<Comment>>,
        next_pr_number: Mutex<u64>,
    }

    impl MockForge {
        fn new() -> Self {
            Self {
                existing_prs: HashMap::new(),
                created_prs: Mutex::new(Vec::new()),
                created_comments: Mutex::new(Vec::new()),
                updated_comments: Mutex::new(Vec::new()),
                updated_bases: Mutex::new(Vec::new()),
                updated_bodies: Mutex::new(Vec::new()),
                deleted_comments: Mutex::new(Vec::new()),
                existing_comments: HashMap::new(),
                next_pr_number: Mutex::new(100),
            }
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
            self.created_prs.lock().unwrap().push(params);
            async move { Ok(pr) }
        }

        fn update_pr_base(
            &self,
            pr_number: u64,
            new_base: &str,
        ) -> impl std::future::Future<Output = Result<(), ForgeError>> + Send {
            self.updated_bases
                .lock()
                .unwrap()
                .push((pr_number, new_base.to_string()));
            async { Ok(()) }
        }

        fn list_comments(
            &self,
            pr_number: u64,
        ) -> impl std::future::Future<Output = Result<Vec<Comment>, ForgeError>> + Send {
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
    }

    impl MockJjRunner {
        fn new() -> (Self, PushLog) {
            let calls: PushLog = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    push_calls: Arc::clone(&calls),
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
        let plan = create_submission_plan(&analysis, &forge, "origin", false)
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

        let plan = create_submission_plan(&analysis, &forge, "origin", false)
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

        let plan = create_submission_plan(&analysis, &forge, "origin", false)
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

        let plan = create_submission_plan(&analysis, &forge, "origin", false)
            .await
            .unwrap();

        assert!(!plan.bookmark_plans[0].needs_create);
        assert!(plan.bookmark_plans[1].needs_create);
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
            default_branch: "main".to_string(),
        };

        let output = plan.to_string();
        assert!(output.contains("2 bookmark(s)"));
        assert!(output.contains("feat-a (base: main)"));
        assert!(output.contains("create PR: \"feature a\""));
        assert!(output.contains("push bookmark to origin"));
        assert!(output.contains("update PR #42 base: main -> feat-a"));
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
            }],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "my-remote".to_string(),
            draft: false,
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
            }],
            remote: "origin".to_string(),
            draft: true,
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
            }],
            remote: "origin".to_string(),
            draft: true,
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
            files: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits);
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
            files: vec![],
            short_change_id: "ch1".to_string(),
        }];

        let body = build_pr_body(&commits);
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
                files: vec![],
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
                files: vec![],
                short_change_id: "ch2".to_string(),
            },
        ];

        let body = build_pr_body(&commits);
        assert_eq!(
            body.as_deref(),
            Some("First commit\n\n---\n\nSecond commit")
        );
    }

    #[test]
    fn build_pr_body_empty() {
        let body = build_pr_body(&[]);
        assert_eq!(body, None);
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
                },
            ],
            remote: "origin".to_string(),
            draft: false,
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
            }],
            remote: "origin".to_string(),
            draft: false,
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
            }],
            remote: "origin".to_string(),
            draft: false,
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
            }],
            remote: "origin".to_string(),
            draft: false,
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
}
