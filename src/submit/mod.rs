//! Three-phase submission: analyze, plan, execute.
//!
//! Takes a change graph and forge implementation and submits bookmarks as
//! stacked pull requests, updating existing PRs idempotently.

use std::fmt;

use anyhow::Context;
use anyhow::Result;

use crate::forge::CreatePrParams;
use crate::forge::Forge;
use crate::forge::PullRequest;
use crate::forge::comment::StackCommentData;
use crate::forge::comment::StackEntry;
use crate::forge::comment::find_stack_comment;
use crate::forge::comment::format_stack_comment;
use crate::graph::types::BookmarkSegment;
use crate::graph::types::ChangeGraph;
use crate::jj::Jj;
use crate::jj::runner::JjRunner;

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
) -> Result<SubmissionAnalysis> {
    let stack = change_graph
        .stacks
        .iter()
        .find(|s| {
            s.segments
                .iter()
                .any(|seg| seg.bookmark_names.contains(&target_bookmark.to_string()))
        })
        .with_context(|| format!("bookmark '{target_bookmark}' not found in any stack"))?;

    let target_index = stack
        .segments
        .iter()
        .position(|seg| seg.bookmark_names.contains(&target_bookmark.to_string()))
        .expect("bookmark was found in stack above");

    let segments = stack.segments[..=target_index].to_vec();

    Ok(SubmissionAnalysis {
        segments,
        default_branch: default_branch.to_string(),
    })
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
) -> Result<SubmissionPlan> {
    let mut bookmark_plans = Vec::new();

    for (i, segment) in analysis.segments.iter().enumerate() {
        let bookmark_name = segment
            .bookmark_names
            .first()
            .context("segment has no bookmark name")?
            .clone();

        let base = if i == 0 {
            analysis.default_branch.clone()
        } else {
            analysis.segments[i - 1]
                .bookmark_names
                .first()
                .context("parent segment has no bookmark name")?
                .clone()
        };

        let title = segment
            .commits
            .first()
            .map(|c| {
                c.description
                    .lines()
                    .next()
                    .unwrap_or(&c.description)
                    .to_string()
            })
            .unwrap_or_else(|| bookmark_name.clone());

        let existing_pr = forge
            .find_pr_for_branch(&bookmark_name)
            .await
            .with_context(|| format!("failed to check for existing PR for '{bookmark_name}'"))?;

        let needs_base_update = existing_pr.as_ref().is_some_and(|pr| pr.base_ref != base);

        let needs_create = existing_pr.is_none();

        bookmark_plans.push(BookmarkPlan {
            bookmark_name,
            base,
            title,
            existing_pr,
            needs_push: true,
            needs_create,
            needs_base_update,
        });
    }

    Ok(SubmissionPlan {
        bookmark_plans,
        remote: remote.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Phase 2: Display (for --dry-run)
// ---------------------------------------------------------------------------

impl fmt::Display for SubmissionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Submission plan ({} bookmark(s), remote: {}):",
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
) -> Result<SubmissionResult> {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    let mut stack_entries = Vec::new();

    // Step 1: Push all bookmarks.
    for bp in &plan.bookmark_plans {
        if bp.needs_push {
            pb.set_message(format!("Pushing bookmark: {}", bp.bookmark_name));
            jj.push_bookmark(&bp.bookmark_name, &plan.remote)
                .await
                .with_context(|| format!("failed to push bookmark '{}'", bp.bookmark_name))?;
        }
    }

    // Step 2: Update bases for existing PRs, then create new PRs.
    for bp in &plan.bookmark_plans {
        let pr = if let Some(existing) = &bp.existing_pr {
            if bp.needs_base_update {
                pb.set_message(format!(
                    "Updating base of PR #{}: {} -> {}",
                    existing.number, existing.base_ref, bp.base,
                ));
                forge
                    .update_pr_base(existing.number, &bp.base)
                    .await
                    .with_context(|| {
                        format!("failed to update PR base for '{}'", bp.bookmark_name,)
                    })?;
            }
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
                    body: None,
                    draft: false,
                })
                .await
                .with_context(|| format!("failed to create PR for '{}'", bp.bookmark_name))?;
            pb.println(format!("  Created PR #{}: {}", pr.number, pr.html_url,));
            pr
        };

        stack_entries.push(StackEntry {
            bookmark_name: bp.bookmark_name.clone(),
            pr_url: pr.html_url.clone(),
            pr_number: pr.number,
        });
    }

    // Step 3: Create/update stack comments on all PRs.
    let comment_data = StackCommentData {
        version: 0,
        stack: stack_entries.clone(),
    };

    for (i, entry) in stack_entries.iter().enumerate() {
        pb.set_message(format!("Updating stack comment on PR #{}", entry.pr_number,));

        let body = format_stack_comment(&comment_data, i);
        let existing_comments = forge
            .list_comments(entry.pr_number)
            .await
            .with_context(|| format!("failed to list comments on PR #{}", entry.pr_number,))?;

        if let Some(existing) = find_stack_comment(&existing_comments) {
            forge
                .update_comment(existing.id, &body)
                .await
                .with_context(|| {
                    format!("failed to update stack comment on PR #{}", entry.pr_number,)
                })?;
        } else {
            forge
                .create_comment(entry.pr_number, &body)
                .await
                .with_context(|| {
                    format!("failed to create stack comment on PR #{}", entry.pr_number,)
                })?;
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
    use crate::graph::types::BranchStack;
    use crate::graph::types::SegmentCommit;
    use crate::jj::JjError;

    // -- Test helpers --

    fn make_segment(names: &[&str], change_id: &str, desc: &str) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(|s| s.to_string()).collect(),
            change_id: change_id.to_string(),
            commits: vec![SegmentCommit {
                commit_id: format!("c_{change_id}"),
                change_id: change_id.to_string(),
                description: desc.to_string(),
                author_name: "Test".to_string(),
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
        }
    }

    // -- Mock Forge --

    struct MockForge {
        existing_prs: HashMap<String, PullRequest>,
        created_prs: Mutex<Vec<CreatePrParams>>,
        created_comments: Mutex<Vec<(u64, String)>>,
        updated_comments: Mutex<Vec<(u64, String)>>,
        updated_bases: Mutex<Vec<(u64, String)>>,
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

        let result = analyze_submission("feat-a", &graph, "main").unwrap();
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

        let result = analyze_submission("feat-b", &graph, "main").unwrap();
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

        let result = analyze_submission("feat-b", &graph, "main").unwrap();
        assert_eq!(result.segments.len(), 2);
    }

    #[test]
    fn analyze_bookmark_not_found() {
        let seg = make_segment(&["feat-a"], "ch_a", "feature a");
        let graph = make_graph(vec![BranchStack {
            segments: vec![seg],
        }]);

        let result = analyze_submission("nonexistent", &graph, "main");
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

        let result = analyze_submission("gamma", &graph, "main").unwrap();
        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.segments[0].bookmark_names, vec!["beta"]);
        assert_eq!(result.segments[1].bookmark_names, vec!["gamma"]);
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
        let plan = create_submission_plan(&analysis, &forge, "origin")
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

        let plan = create_submission_plan(&analysis, &forge, "origin")
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

        let plan = create_submission_plan(&analysis, &forge, "origin")
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

        let plan = create_submission_plan(&analysis, &forge, "origin")
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
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    existing_pr: Some(make_pr(42, "feat-b", "main")),
                    needs_push: true,
                    needs_create: false,
                    needs_base_update: true,
                },
            ],
            remote: "origin".to_string(),
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
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
            ],
            remote: "origin".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();

        let result = execute_submission_plan(&plan, &jj, &forge).await.unwrap();

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
                existing_pr: Some(make_pr(42, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: true,
            }],
            remote: "origin".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();

        execute_submission_plan(&plan, &jj, &forge).await.unwrap();

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
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
            ],
            remote: "origin".to_string(),
        };

        let (runner, _push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();

        execute_submission_plan(&plan, &jj, &forge).await.unwrap();

        let comments = forge.created_comments.lock().unwrap();
        // One stack comment per PR.
        assert_eq!(comments.len(), 2);
        // Comments should contain JACK_STACK metadata.
        assert!(comments[0].1.contains("JACK_STACK"));
        assert!(comments[1].1.contains("JACK_STACK"));
    }

    #[tokio::test]
    async fn execute_updates_existing_stack_comments() {
        let existing_comment_body = format_stack_comment(
            &StackCommentData {
                version: 0,
                stack: vec![StackEntry {
                    bookmark_name: "old".to_string(),
                    pr_url: "https://example.com/1".to_string(),
                    pr_number: 1,
                }],
            },
            0,
        );

        let plan = SubmissionPlan {
            bookmark_plans: vec![BookmarkPlan {
                bookmark_name: "feat-a".to_string(),
                base: "main".to_string(),
                title: "feature a".to_string(),
                existing_pr: Some(make_pr(50, "feat-a", "main")),
                needs_push: true,
                needs_create: false,
                needs_base_update: false,
            }],
            remote: "origin".to_string(),
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

        execute_submission_plan(&plan, &jj, &forge).await.unwrap();

        // Should have updated the existing comment, not created a new one.
        let created = forge.created_comments.lock().unwrap();
        assert_eq!(created.len(), 0);

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
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
                BookmarkPlan {
                    bookmark_name: "feat-b".to_string(),
                    base: "feat-a".to_string(),
                    title: "feature b".to_string(),
                    existing_pr: None,
                    needs_push: true,
                    needs_create: true,
                    needs_base_update: false,
                },
            ],
            remote: "my-remote".to_string(),
        };

        let (runner, push_calls) = MockJjRunner::new();
        let jj = Jj::new(runner);
        let forge = MockForge::new();

        execute_submission_plan(&plan, &jj, &forge).await.unwrap();

        let calls = push_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], ("feat-a".to_string(), "my-remote".to_string()));
        assert_eq!(calls[1], ("feat-b".to_string(), "my-remote".to_string()));
    }
}
