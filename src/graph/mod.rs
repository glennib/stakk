//! Change graph construction.
//!
//! Builds `ChangeGraph`, `BookmarkSegment`, and `BranchStack` from jj output to
//! determine the stacking order of bookmarks for PR submission.

pub mod types;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;

use self::types::BookmarkSegment;
use self::types::BranchStack;
use self::types::ChangeGraph;
use self::types::SegmentCommit;
use crate::error::StakkError;
use crate::jj::Jj;
use crate::jj::runner::JjRunner;
use crate::jj::types::Bookmark;

/// Result of traversing from one bookmark toward trunk.
struct TraversalResult {
    /// Discovered segments, ordered newest-first (leaf toward trunk).
    segments: Vec<BookmarkSegment>,
    /// If traversal stopped because it hit an already-collected bookmark,
    /// this is that bookmark's change_id.
    already_seen_change_id: Option<String>,
    /// Whether this bookmark was excluded (tainted by a merge commit).
    excluded: bool,
}

/// Build the complete change graph from the current jj repo state.
///
/// Discovers all user bookmarks, traverses each toward trunk to find segments,
/// builds an adjacency list, detects merge commits, identifies leaves, and
/// groups segments into stacks.
pub async fn build_change_graph<R: JjRunner>(jj: &Jj<R>) -> Result<ChangeGraph, StakkError> {
    let bookmarks = jj.get_my_bookmarks().await?;

    // Collect user bookmark names so traversal can filter out non-user bookmarks
    // that appear on commits (e.g. bookmarks from other users).
    let user_bookmark_names: HashSet<String> = bookmarks.iter().map(|b| b.name.clone()).collect();

    let mut fully_collected: HashSet<String> = HashSet::new();
    let mut adjacency_list: HashMap<String, String> = HashMap::new();
    let mut segments: HashMap<String, BookmarkSegment> = HashMap::new();
    let mut stack_roots: HashSet<String> = HashSet::new();
    let mut tainted_change_ids: HashSet<String> = HashSet::new();
    let mut excluded_bookmark_count: usize = 0;

    for bookmark in &bookmarks {
        if fully_collected.contains(&bookmark.name) {
            continue;
        }

        let result = traverse_and_discover_segments(
            bookmark,
            jj,
            &fully_collected,
            &mut tainted_change_ids,
            &user_bookmark_names,
        )
        .await?;

        if result.excluded {
            excluded_bookmark_count += 1;
            continue;
        }

        // Store segments and mark bookmark names as fully collected.
        for seg in &result.segments {
            for name in &seg.bookmark_names {
                fully_collected.insert(name.clone());
            }
        }

        // Build adjacency: consecutive segments are child -> parent.
        // result.segments is ordered newest-first (leaf toward trunk).
        for window in result.segments.windows(2) {
            let child_id = &window[0].change_id;
            let parent_id = &window[1].change_id;
            adjacency_list.insert(child_id.clone(), parent_id.clone());
        }

        // Connect to already-seen segment if traversal stopped early.
        if let Some(ref seen_id) = result.already_seen_change_id {
            if let Some(last_seg) = result.segments.last() {
                adjacency_list.insert(last_seg.change_id.clone(), seen_id.clone());
            }
        } else if let Some(last_seg) = result.segments.last() {
            // Reached trunk: this is a root.
            stack_roots.insert(last_seg.change_id.clone());
        }

        for seg in result.segments {
            segments.insert(seg.change_id.clone(), seg);
        }
    }

    // Identify leaves: segments not pointed to as parent by anyone.
    let parent_ids: HashSet<&String> = adjacency_list.values().collect();
    let stack_leaves: HashSet<String> = segments
        .keys()
        .filter(|id| !parent_ids.contains(id))
        .cloned()
        .collect();

    let stacks = group_segments_into_stacks(&stack_leaves, &adjacency_list, &segments);

    Ok(ChangeGraph {
        adjacency_list,
        stack_leaves,
        stack_roots,
        segments,
        tainted_change_ids,
        excluded_bookmark_count,
        stacks,
    })
}

/// Traverse from a bookmark toward trunk, discovering segments along the way.
///
/// Fetches commits in pages of 100. At each commit, checks for local bookmarks
/// to determine segment boundaries. Stops when:
/// - hitting a commit whose bookmark was already fully collected
/// - reaching trunk (no more commits in the revset)
/// - encountering a merge commit (taints this bookmark)
async fn traverse_and_discover_segments<R: JjRunner>(
    bookmark: &Bookmark,
    jj: &Jj<R>,
    fully_collected: &HashSet<String>,
    tainted_change_ids: &mut HashSet<String>,
    user_bookmark_names: &HashSet<String>,
) -> Result<TraversalResult, StakkError> {
    let mut segments: Vec<BookmarkSegment> = Vec::new();
    let mut current_segment: Option<BookmarkSegment> = None;
    let mut last_seen_commit: Option<String> = None;
    let mut already_seen_change_id: Option<String> = None;
    let mut seen_change_ids: Vec<String> = Vec::new();

    'page_loop: loop {
        let changes = jj
            .get_branch_changes_paginated(
                "trunk()",
                &bookmark.commit_id,
                last_seen_commit.as_deref(),
            )
            .await?;

        if changes.is_empty() {
            break;
        }

        for change in &changes {
            seen_change_ids.push(change.change_id.clone());

            // Detect merge commits or already-tainted changes.
            if change.parents.len() > 1 || tainted_change_ids.contains(&change.change_id) {
                for id in &seen_change_ids {
                    tainted_change_ids.insert(id.clone());
                }
                return Ok(TraversalResult {
                    segments: Vec::new(),
                    already_seen_change_id: None,
                    excluded: true,
                });
            }

            // Filter to only user-owned bookmarks on this commit.
            let user_bookmarks: Vec<String> = change
                .local_bookmark_names
                .iter()
                .filter(|name| user_bookmark_names.contains(*name))
                .cloned()
                .collect();

            // Check if this commit has user bookmarks (segment boundary).
            if !user_bookmarks.is_empty() {
                // Finish current segment if any.
                if let Some(seg) = current_segment.take() {
                    segments.push(seg);
                }

                // Check if any bookmark on this change was already collected.
                if user_bookmarks
                    .iter()
                    .any(|name| fully_collected.contains(name))
                {
                    already_seen_change_id = Some(change.change_id.clone());
                    break 'page_loop;
                }

                // Start new segment.
                current_segment = Some(BookmarkSegment {
                    bookmark_names: user_bookmarks,
                    change_id: change.change_id.clone(),
                    commits: Vec::new(),
                });
            }

            // Add commit to current segment.
            if let Some(ref mut seg) = current_segment {
                seg.commits.push(SegmentCommit {
                    commit_id: change.commit_id.clone(),
                    change_id: change.change_id.clone(),
                    description: change.description.clone(),
                    author_name: change.author.name.clone(),
                });
            } else {
                // Commit before any bookmark — shouldn't happen because the
                // first entry in trunk()..bookmark should be the bookmark
                // commit itself.
                return Err(StakkError::Graph {
                    message: format!(
                        "encountered change {} before any bookmark while traversing from bookmark \
                         '{}'",
                        change.change_id, bookmark.name,
                    ),
                });
            }
        }

        if changes.len() < 100 {
            break; // Last page.
        }

        last_seen_commit = changes.last().map(|c| c.commit_id.clone());
    }

    // Push final segment.
    if let Some(seg) = current_segment {
        segments.push(seg);
    }

    Ok(TraversalResult {
        segments,
        already_seen_change_id,
        excluded: false,
    })
}

/// Walk from each leaf to root via the adjacency list, producing one
/// `BranchStack` per leaf. Each stack is ordered trunk-to-leaf (bottom first).
fn group_segments_into_stacks(
    stack_leaves: &HashSet<String>,
    adjacency_list: &HashMap<String, String>,
    segments: &HashMap<String, BookmarkSegment>,
) -> Vec<BranchStack> {
    let mut stacks = Vec::new();

    // Sort leaves for deterministic output.
    let mut leaves: Vec<&String> = stack_leaves.iter().collect();
    leaves.sort();

    for leaf_id in leaves {
        let mut path = vec![leaf_id.clone()];
        let mut current = leaf_id.clone();

        while let Some(parent) = adjacency_list.get(&current) {
            path.push(parent.clone());
            current = parent.clone();
        }

        // Reverse so trunk end is first.
        path.reverse();

        let stack_segments: Vec<BookmarkSegment> = path
            .iter()
            .filter_map(|id| segments.get(id).cloned())
            .collect();

        stacks.push(BranchStack {
            segments: stack_segments,
        });
    }

    stacks
}

/// Topological sort using Kahn's algorithm.
///
/// Returns change IDs ordered leaves-first, roots-last. This is the order
/// suitable for display (the user sees their leaf work at the top).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used in later milestones for display ordering")
)]
pub fn topological_sort(graph: &ChangeGraph) -> Vec<String> {
    // Calculate in-degrees: how many children point to each parent.
    let mut in_degrees: HashMap<&String, usize> = HashMap::new();
    for parent_id in graph.adjacency_list.values() {
        *in_degrees.entry(parent_id).or_insert(0) += 1;
    }

    // Start from leaves, sorted for deterministic output.
    let mut leaves: Vec<String> = graph.stack_leaves.iter().cloned().collect();
    leaves.sort();
    let mut queue: VecDeque<String> = leaves.into();

    let mut result = Vec::new();

    while let Some(change_id) = queue.pop_front() {
        result.push(change_id.clone());

        if let Some(parent_id) = graph.adjacency_list.get(&change_id)
            && let Some(degree) = in_degrees.get_mut(parent_id)
        {
            *degree -= 1;
            if *degree == 0 {
                // Parent is now ready — push to front to keep stacks
                // visually grouped (DFS-like).
                queue.push_front(parent_id.clone());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jj::JjError;
    use crate::jj::runner::JjRunner;

    // -- Mock runner (same pattern as jj/mod.rs) --

    struct MockJjRunner<F: Fn(&[&str]) -> Result<String, JjError> + Send + Sync> {
        handler: F,
    }

    impl<F> JjRunner for MockJjRunner<F>
    where
        F: Fn(&[&str]) -> Result<String, JjError> + Send + Sync,
    {
        async fn run_jj(&self, args: &[&str]) -> Result<String, JjError> {
            (self.handler)(args)
        }
    }

    // -- Test helpers --

    /// Build a bookmark list NDJSON line.
    fn bookmark_json(name: &str, commit_id: &str, change_id: &str) -> String {
        format!(
            r#"{{"name":"{name}","synced":false,"target":{{"commit_id":"{commit_id}","parents":[],"change_id":"{change_id}","description":"","author":{{"name":"T","email":"t@t.t","timestamp":"T"}},"committer":{{"name":"T","email":"t@t.t","timestamp":"T"}}}}}}"#,
        )
    }

    /// Build a log entry NDJSON line.
    fn log_entry_json(
        commit_id: &str,
        change_id: &str,
        parents: &[&str],
        local_bookmarks: &[&str],
    ) -> String {
        let parents_json: Vec<String> = parents.iter().map(|p| format!("\"{p}\"")).collect();
        let parents_str = parents_json.join(",");

        let bookmarks_json: Vec<String> = local_bookmarks
            .iter()
            .map(|b| format!(r#"{{"name":"{b}","target":["{commit_id}"]}}"#))
            .collect();
        let bookmarks_str = bookmarks_json.join(",");

        format!(
            r#"{{"commit":{{"commit_id":"{commit_id}","parents":[{parents_str}],"change_id":"{change_id}","description":"desc {commit_id}","author":{{"name":"T","email":"t@t.t","timestamp":"T"}},"committer":{{"name":"T","email":"t@t.t","timestamp":"T"}}}},"local_bookmarks":[{bookmarks_str}],"remote_bookmarks":[]}}"#,
        )
    }

    // -- Tests --

    /// Simple linear stack: trunk -> bm_a -> bm_b
    ///
    /// Bookmark list returns [bm_b, bm_a].
    /// Traversing bm_b: log returns [c_b(bm_b), c_a(bm_a)].
    /// bm_a is already discovered, so traversing bm_a is skipped.
    /// Result: 1 stack with 2 segments [bm_a, bm_b] (trunk-to-leaf).
    #[tokio::test]
    async fn linear_stack() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    // Return two bookmarks.
                    let lines = [
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                // log command for trunk()..c_b
                let revset = args[2];
                if revset.contains("c_b") {
                    let lines = [
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 2);
        assert_eq!(graph.stacks.len(), 1);
        assert_eq!(graph.stack_leaves.len(), 1);
        assert!(graph.stack_leaves.contains("ch_b"));
        assert_eq!(graph.stack_roots.len(), 1);
        assert!(graph.stack_roots.contains("ch_a"));

        // Adjacency: ch_b -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_b").unwrap(), "ch_a");

        // Stack order is trunk-to-leaf: [bm_a, bm_b]
        let stack = &graph.stacks[0];
        assert_eq!(stack.segments.len(), 2);
        assert_eq!(stack.segments[0].bookmark_names, vec!["bm_a"]);
        assert_eq!(stack.segments[1].bookmark_names, vec!["bm_b"]);
    }

    /// Branching: trunk -> bm_a -> bm_b and trunk -> bm_a -> bm_c
    ///
    /// Two stacks sharing a common root (bm_a).
    /// bm_b and bm_c are both leaves.
    #[tokio::test]
    async fn branching_shared_root() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_c", "c_c", "ch_c"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_b") {
                    let lines = [
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }
                if revset.contains("c_c") {
                    // bm_a is already collected, so traversal stops there.
                    let lines = [
                        log_entry_json("c_c", "ch_c", &["c_a"], &["bm_c"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 3);
        assert_eq!(graph.stacks.len(), 2);

        // Both bm_b and bm_c are leaves.
        assert!(graph.stack_leaves.contains("ch_b"));
        assert!(graph.stack_leaves.contains("ch_c"));

        // bm_a is root.
        assert!(graph.stack_roots.contains("ch_a"));

        // Adjacency: ch_b -> ch_a, ch_c -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_b").unwrap(), "ch_a");
        assert_eq!(graph.adjacency_list.get("ch_c").unwrap(), "ch_a");

        // Both stacks start with bm_a.
        for stack in &graph.stacks {
            assert_eq!(stack.segments[0].bookmark_names, vec!["bm_a"]);
            assert_eq!(stack.segments.len(), 2);
        }
    }

    /// Merge commit exclusion: bookmark points at a merge commit (>1 parent).
    /// The bookmark should be excluded and tainted.
    #[tokio::test]
    async fn merge_commit_excluded() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_merge", "c_merge", "ch_merge"));
                }

                let revset = args[2];
                if revset.contains("c_merge") {
                    // Merge commit: two parents.
                    return Ok(log_entry_json(
                        "c_merge",
                        "ch_merge",
                        &["parent_a", "parent_b"],
                        &["bm_merge"],
                    ));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.stacks.len(), 0);
        assert_eq!(graph.excluded_bookmark_count, 1);
        assert!(graph.tainted_change_ids.contains("ch_merge"));
    }

    /// Taint propagation: a descendant of a merge commit is also tainted.
    ///
    /// trunk -> bm_a (merge) -> bm_b
    /// When we traverse bm_b first, we find bm_b, then bm_a which is a merge.
    /// Both get tainted.
    #[tokio::test]
    async fn merge_taint_propagation() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_b") {
                    let lines = [
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        // bm_a is a merge commit.
                        log_entry_json("c_a", "ch_a", &["p1", "p2"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.stacks.len(), 0);
        // bm_b excluded because its traversal hit a merge.
        assert_eq!(graph.excluded_bookmark_count, 1);
        assert!(graph.tainted_change_ids.contains("ch_a"));
        assert!(graph.tainted_change_ids.contains("ch_b"));

        // bm_a is skipped in the outer loop because it's now tainted.
        // The handler for c_a is never called separately.
    }

    /// When a second bookmark traverses and hits the tainted set, it should
    /// also be excluded.
    #[tokio::test]
    async fn taint_from_previous_traversal() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        // bm_merge will be processed first, tainting ch_merge.
                        bookmark_json("bm_merge", "c_merge", "ch_merge"),
                        // bm_child sits on top of the merge.
                        bookmark_json("bm_child", "c_child", "ch_child"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_merge") {
                    return Ok(log_entry_json(
                        "c_merge",
                        "ch_merge",
                        &["p1", "p2"],
                        &["bm_merge"],
                    ));
                }
                if revset.contains("c_child") {
                    let lines = [
                        log_entry_json("c_child", "ch_child", &["c_merge"], &["bm_child"]),
                        // ch_merge is already tainted from bm_merge's traversal.
                        log_entry_json("c_merge", "ch_merge", &["p1", "p2"], &["bm_merge"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.stacks.len(), 0);
        assert_eq!(graph.excluded_bookmark_count, 2);
        assert!(graph.tainted_change_ids.contains("ch_merge"));
        assert!(graph.tainted_change_ids.contains("ch_child"));
    }

    /// Multiple bookmarks on the same change: single segment with both names.
    #[tokio::test]
    async fn multiple_bookmarks_same_change() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_a", "c_x", "ch_x"),
                        bookmark_json("bm_b", "c_x", "ch_x"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_x") {
                    // Both bookmarks appear on the same commit.
                    return Ok(log_entry_json(
                        "c_x",
                        "ch_x",
                        &["trunk_c"],
                        &["bm_a", "bm_b"],
                    ));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);

        let seg = graph.segments.get("ch_x").unwrap();
        assert_eq!(seg.bookmark_names.len(), 2);
        assert!(seg.bookmark_names.contains(&"bm_a".to_string()));
        assert!(seg.bookmark_names.contains(&"bm_b".to_string()));
    }

    /// No bookmarks: empty graph.
    #[tokio::test]
    async fn no_bookmarks() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    return Ok(String::new());
                }
                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert!(graph.segments.is_empty());
        assert!(graph.stacks.is_empty());
        assert!(graph.stack_leaves.is_empty());
        assert!(graph.stack_roots.is_empty());
        assert_eq!(graph.excluded_bookmark_count, 0);
    }

    /// Multi-commit segment: unbookmarked commits between bookmarks are
    /// included in the parent-ward segment.
    ///
    /// trunk -> c1 -> c2(bm_a) -> c3 -> c4(bm_b)
    ///
    /// Segment bm_b should contain [c4, c3] (newest first).
    /// Segment bm_a should contain [c2, c1].
    #[tokio::test]
    async fn multi_commit_segment() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_b", "c4", "ch_b"),
                        bookmark_json("bm_a", "c2", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c4") {
                    let lines = [
                        log_entry_json("c4", "ch_b", &["c3"], &["bm_b"]),
                        log_entry_json("c3", "ch_3", &["c2"], &[]),
                        log_entry_json("c2", "ch_a", &["c1"], &["bm_a"]),
                        log_entry_json("c1", "ch_1", &["trunk_c"], &[]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 2);
        assert_eq!(graph.stacks.len(), 1);

        let seg_b = graph.segments.get("ch_b").unwrap();
        assert_eq!(seg_b.commits.len(), 2);
        assert_eq!(seg_b.commits[0].commit_id, "c4");
        assert_eq!(seg_b.commits[1].commit_id, "c3");

        let seg_a = graph.segments.get("ch_a").unwrap();
        assert_eq!(seg_a.commits.len(), 2);
        assert_eq!(seg_a.commits[0].commit_id, "c2");
        assert_eq!(seg_a.commits[1].commit_id, "c1");

        // Stack order: [bm_a, bm_b]
        let stack = &graph.stacks[0];
        assert_eq!(stack.segments[0].change_id, "ch_a");
        assert_eq!(stack.segments[1].change_id, "ch_b");
    }

    /// Already-collected bookmark: second traversal connects to first via
    /// adjacency list without duplicating the segment.
    ///
    /// Bookmarks [bm_b, bm_c, bm_a] where:
    ///   trunk -> bm_a -> bm_b
    ///   trunk -> bm_a -> bm_c
    ///
    /// Traversing bm_b discovers [bm_b, bm_a].
    /// Traversing bm_c discovers [bm_c], stops at bm_a (already collected).
    /// bm_a is NOT traversed separately (already collected).
    #[tokio::test]
    async fn already_collected_early_stop() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_c", "c_c", "ch_c"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_b") {
                    let lines = [
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }
                if revset.contains("c_c") {
                    let lines = [
                        log_entry_json("c_c", "ch_c", &["c_a"], &["bm_c"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                // Should NOT be called for c_a because bm_a is already
                // collected.
                panic!("unexpected revset: {revset}");
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        // bm_a segment is NOT duplicated.
        assert_eq!(graph.segments.len(), 3);
        assert_eq!(graph.stacks.len(), 2);

        // Adjacency: ch_b -> ch_a, ch_c -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_b").unwrap(), "ch_a");
        assert_eq!(graph.adjacency_list.get("ch_c").unwrap(), "ch_a");
    }

    /// Topological sort: leaves first, roots last.
    ///
    /// Graph: ch_c -> ch_b -> ch_a (linear)
    /// Expected sort: [ch_c, ch_b, ch_a]
    #[tokio::test]
    async fn topological_sort_linear() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_c", "c_c", "ch_c"),
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_c") {
                    let lines = [
                        log_entry_json("c_c", "ch_c", &["c_b"], &["bm_c"]),
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();
        let sorted = topological_sort(&graph);

        assert_eq!(sorted, vec!["ch_c", "ch_b", "ch_a"]);
    }

    /// Topological sort with branching: leaves processed first, then shared
    /// root.
    ///
    /// Graph: ch_b -> ch_a, ch_c -> ch_a
    /// Expected: [ch_b, ch_c, ch_a] or [ch_c, ch_b, ch_a] depending on sort.
    /// With alphabetical leaf ordering: ch_b before ch_c.
    #[tokio::test]
    async fn topological_sort_branching() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    let lines = [
                        bookmark_json("bm_b", "c_b", "ch_b"),
                        bookmark_json("bm_c", "c_c", "ch_c"),
                        bookmark_json("bm_a", "c_a", "ch_a"),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_b") {
                    let lines = [
                        log_entry_json("c_b", "ch_b", &["c_a"], &["bm_b"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }
                if revset.contains("c_c") {
                    let lines = [
                        log_entry_json("c_c", "ch_c", &["c_a"], &["bm_c"]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();
        let sorted = topological_sort(&graph);

        // ch_b is processed first (alphabetical), its parent ch_a becomes
        // ready but ch_c hasn't been processed yet. Since we push_front the
        // parent, ch_a goes to front, then ch_c is next in queue.
        // Actually with push_front: queue starts [ch_b, ch_c].
        // Pop ch_b -> result [ch_b]. Parent ch_a has in-degree 2, decrement
        // to 1 — not ready yet. Queue: [ch_c].
        // Pop ch_c -> result [ch_b, ch_c]. Parent ch_a decremented to 0,
        // push_front. Queue: [ch_a].
        // Pop ch_a -> result [ch_b, ch_c, ch_a].
        assert_eq!(sorted, vec!["ch_b", "ch_c", "ch_a"]);
    }

    /// Single bookmark, single commit — simplest possible case.
    #[tokio::test]
    async fn single_bookmark_single_commit() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_x", "c_x", "ch_x"));
                }

                let revset = args[2];
                if revset.contains("c_x") {
                    return Ok(log_entry_json("c_x", "ch_x", &["trunk_c"], &["bm_x"]));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);
        assert!(graph.stack_leaves.contains("ch_x"));
        assert!(graph.stack_roots.contains("ch_x"));
        assert!(graph.adjacency_list.is_empty());

        let stack = &graph.stacks[0];
        assert_eq!(stack.segments.len(), 1);
        assert_eq!(stack.segments[0].bookmark_names, vec!["bm_x"]);
        assert_eq!(stack.segments[0].commits.len(), 1);
        assert_eq!(stack.segments[0].commits[0].commit_id, "c_x");
    }

    /// Verify segment commit metadata is correctly populated.
    #[tokio::test]
    async fn segment_commit_metadata() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("feat", "c1", "ch1"));
                }

                let revset = args[2];
                if revset.contains("c1") {
                    return Ok(log_entry_json("c1", "ch1", &["trunk_c"], &["feat"]));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        let seg = graph.segments.get("ch1").unwrap();
        assert_eq!(seg.commits[0].commit_id, "c1");
        assert_eq!(seg.commits[0].change_id, "ch1");
        assert_eq!(seg.commits[0].description, "desc c1");
        assert_eq!(seg.commits[0].author_name, "T");
    }

    /// group_segments_into_stacks is deterministic (sorted by leaf change_id).
    #[test]
    fn stacks_are_deterministically_ordered() {
        let mut segments = HashMap::new();
        let adjacency_list = HashMap::new();
        let mut stack_leaves = HashSet::new();

        // Three independent leaves (no shared root).
        for id in ["z_leaf", "a_leaf", "m_leaf"] {
            segments.insert(
                id.to_string(),
                BookmarkSegment {
                    bookmark_names: vec![id.to_string()],
                    change_id: id.to_string(),
                    commits: vec![],
                },
            );
            stack_leaves.insert(id.to_string());
        }

        let stacks = group_segments_into_stacks(&stack_leaves, &adjacency_list, &segments);

        assert_eq!(stacks.len(), 3);
        // Sorted alphabetically by leaf change_id.
        assert_eq!(stacks[0].segments[0].change_id, "a_leaf");
        assert_eq!(stacks[1].segments[0].change_id, "m_leaf");
        assert_eq!(stacks[2].segments[0].change_id, "z_leaf");
    }

    /// Non-user bookmarks on a commit are filtered out; segment uses only
    /// user-owned bookmarks.
    ///
    /// Commit c_x has bookmarks [bm_user, bm_other]. Only bm_user is returned
    /// by get_my_bookmarks(), so the segment should contain only bm_user.
    #[tokio::test]
    async fn non_user_bookmarks_filtered_from_segment() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    // Only bm_user belongs to the user.
                    return Ok(bookmark_json("bm_user", "c_x", "ch_x"));
                }

                let revset = args[2];
                if revset.contains("c_x") {
                    // The commit has both a user bookmark and a non-user
                    // bookmark.
                    return Ok(log_entry_json(
                        "c_x",
                        "ch_x",
                        &["trunk_c"],
                        &["bm_user", "bm_other"],
                    ));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        assert_eq!(graph.segments.len(), 1);
        let seg = graph.segments.get("ch_x").unwrap();
        assert_eq!(seg.bookmark_names, vec!["bm_user"]);
    }

    /// A commit with only non-user bookmarks is treated as unbookmarked
    /// (no segment boundary).
    ///
    /// trunk -> c_other(bm_other) -> c_user(bm_user)
    /// Only bm_user is the user's bookmark. c_other has only bm_other, so
    /// it should be treated as an unbookmarked commit within bm_user's segment.
    #[tokio::test]
    async fn only_non_user_bookmarks_no_segment_boundary() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_user", "c_user", "ch_user"));
                }

                let revset = args[2];
                if revset.contains("c_user") {
                    let lines = [
                        log_entry_json("c_user", "ch_user", &["c_other"], &["bm_user"]),
                        // bm_other is not a user bookmark → no segment boundary.
                        log_entry_json("c_other", "ch_other", &["trunk_c"], &["bm_other"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(&jj).await.unwrap();

        // Only one segment (bm_user), containing both commits.
        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);

        let seg = graph.segments.get("ch_user").unwrap();
        assert_eq!(seg.bookmark_names, vec!["bm_user"]);
        assert_eq!(seg.commits.len(), 2);
        assert_eq!(seg.commits[0].commit_id, "c_user");
        assert_eq!(seg.commits[1].commit_id, "c_other");
    }
}
