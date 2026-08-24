//! Change graph construction and rendering.
//!
//! Builds `ChangeGraph`, `BookmarkSegment`, and `BranchStack` from jj output to
//! determine the stacking order of bookmarks for PR submission. [`layout`]
//! turns a graph into display rows; [`output`] renders the `stakk graph`
//! subcommand's two formats on top of them.

pub mod layout;
pub mod output;
pub mod types;

use std::collections::HashMap;
use std::collections::HashSet;

use jiff::Timestamp;

use self::types::BookmarkSegment;
use self::types::BranchStack;
use self::types::ChangeGraph;
use self::types::RemoteState;
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
    /// this is that bookmark's `change_id`.
    already_seen_change_id: Option<String>,
    /// Whether this bookmark was excluded (tainted by a merge commit).
    excluded: bool,
}

/// Build the complete change graph from the current jj repo state.
///
/// Discovers all user bookmarks, traverses each toward trunk to find segments,
/// builds an adjacency list, detects merge commits, identifies leaves, and
/// groups segments into stacks.
pub async fn build_change_graph<R: JjRunner>(
    jj: &Jj<R>,
    bookmarks_revset: &str,
    heads_revset: &str,
) -> Result<ChangeGraph, StakkError> {
    let bookmarks = jj.get_my_bookmarks(bookmarks_revset).await?;

    // Collect user bookmark names so traversal can filter out non-user bookmarks
    // that appear on commits (e.g. bookmarks from other users).
    let user_bookmark_names: HashSet<String> = bookmarks.iter().map(|b| b.name.clone()).collect();

    let mut fully_collected: HashSet<String> = HashSet::new();
    let mut adjacency_list: HashMap<String, String> = HashMap::new();
    let mut segments: HashMap<String, BookmarkSegment> = HashMap::new();
    let mut tainted_change_ids: HashSet<String> = HashSet::new();
    let mut excluded_bookmarks: Vec<String> = Vec::new();
    let mut excluded_head_count: usize = 0;

    for bookmark in &bookmarks {
        if fully_collected.contains(&bookmark.name) {
            continue;
        }

        let result = traverse_and_discover_segments(
            &bookmark.commit_id,
            jj,
            &fully_collected,
            &mut tainted_change_ids,
            &user_bookmark_names,
        )
        .await?;

        if result.excluded {
            excluded_bookmarks.push(bookmark.name.clone());
            continue;
        }

        integrate_traversal_result(
            result,
            &mut adjacency_list,
            &mut segments,
            &mut fully_collected,
        );
    }

    // Discover unbookmarked heads — changes beyond the last bookmark.
    let bookmarked_commit_ids: HashSet<String> =
        bookmarks.iter().map(|b| b.commit_id.clone()).collect();

    let heads = jj.get_heads(heads_revset).await?;
    for head in &heads {
        // Skip heads that are at a bookmarked commit (already traversed).
        if bookmarked_commit_ids.contains(&head.commit_id) {
            continue;
        }
        // Skip heads whose change_id is already in segments.
        if segments.contains_key(&head.change_id) {
            continue;
        }

        let result = traverse_and_discover_segments(
            &head.commit_id,
            jj,
            &fully_collected,
            &mut tainted_change_ids,
            &user_bookmark_names,
        )
        .await?;

        if result.excluded {
            excluded_head_count += 1;
            continue;
        }

        integrate_traversal_result(
            result,
            &mut adjacency_list,
            &mut segments,
            &mut fully_collected,
        );
    }

    // Identify leaves: segments not pointed to as parent by anyone.
    let parent_ids: HashSet<&String> = adjacency_list.values().collect();
    let stack_leaves: HashSet<String> = segments
        .keys()
        .filter(|id| !parent_ids.contains(id))
        .cloned()
        .collect();

    let mut stacks = group_segments_into_stacks(&stack_leaves, &adjacency_list, &segments);

    // Pre-fetch file lists for all commits concurrently.
    fetch_file_lists(jj, &mut stacks).await?;
    // Also update the segments map so it stays in sync.
    for stack in &stacks {
        for segment in &stack.segments {
            if let Some(seg) = segments.get_mut(&segment.change_id) {
                *seg = segment.clone();
            }
        }
    }

    let bookmark_remote_states = derive_remote_states(&bookmarks, &stacks);

    Ok(ChangeGraph {
        adjacency_list,
        stack_leaves,
        segments,
        tainted_change_ids,
        bookmark_remote_states,
        excluded_bookmarks,
        excluded_head_count,
        stacks,
    })
}

/// Classify every bookmark that reached a segment as unpushed, diverged or
/// synced.
///
/// Two facts decide it, and neither is enough alone. `jj`'s `synced()` is
/// false only when a *tracked* remote disagrees with the local bookmark, so a
/// never-pushed bookmark reports `synced = true` exactly like an up-to-date
/// one. What separates those two is whether a remote bookmark of the same
/// name sits on the segment's boundary commit — which is where it must be if
/// the two agree.
///
/// The internal `name@git` remote is not a real remote and is skipped: it
/// tracks the colocated git repo, not anything a push would reach.
fn derive_remote_states(
    bookmarks: &[Bookmark],
    stacks: &[BranchStack],
) -> HashMap<String, RemoteState> {
    let synced: HashMap<&str, bool> = bookmarks
        .iter()
        .map(|b| (b.name.as_str(), b.synced))
        .collect();

    let mut states = HashMap::new();
    for stack in stacks {
        for segment in &stack.segments {
            // The boundary commit is the one the bookmarks point at, stored
            // first because segment commits are newest-first.
            let Some(boundary) = segment.commits.first() else {
                continue;
            };
            for name in &segment.bookmark_names {
                let state = if !synced.get(name.as_str()).copied().unwrap_or(true) {
                    RemoteState::Diverged
                } else if has_real_remote(&boundary.remote_bookmark_names, name) {
                    RemoteState::Synced
                } else {
                    RemoteState::Unpushed
                };
                states.insert(name.clone(), state);
            }
        }
    }
    states
}

/// Whether `remote_names` contains `<name>@<remote>` for a remote other than
/// jj's internal `git`.
fn has_real_remote(remote_names: &[String], name: &str) -> bool {
    remote_names.iter().any(|entry| {
        entry
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('@'))
            .is_some_and(|remote| remote != "git")
    })
}

/// Integrate a traversal result into the shared graph state.
///
/// Stores discovered segments, builds adjacency relationships, and marks
/// bookmark names as fully collected.
fn integrate_traversal_result(
    result: TraversalResult,
    adjacency_list: &mut HashMap<String, String>,
    segments: &mut HashMap<String, BookmarkSegment>,
    fully_collected: &mut HashSet<String>,
) {
    // Mark bookmark names as fully collected.
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

    // Connect to already-seen segment if traversal stopped early. Otherwise
    // traversal reached trunk and the last segment has no parent, which is
    // what makes it a stack root — recorded implicitly by its absence from
    // the adjacency list.
    if let Some(ref seen_id) = result.already_seen_change_id
        && let Some(last_seg) = result.segments.last()
    {
        adjacency_list.insert(last_seg.change_id.clone(), seen_id.clone());
    }

    for seg in result.segments {
        segments.insert(seg.change_id.clone(), seg);
    }
}

/// Traverse from a starting commit toward trunk, discovering segments along the
/// way.
///
/// Fetches commits in pages of 100. At each commit, checks for local bookmarks
/// to determine segment boundaries. Stops when:
/// - hitting a commit whose bookmark was already fully collected
/// - reaching trunk (no more commits in the revset)
/// - encountering a merge commit (taints this traversal)
///
/// `start_commit_id` is the commit to begin traversal from (a bookmark target
/// or an unbookmarked head).
async fn traverse_and_discover_segments<R: JjRunner>(
    start_commit_id: &str,
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
            .get_branch_changes_paginated("trunk()", start_commit_id, last_seen_commit.as_deref())
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

            // Add commit to current segment. If no segment exists yet
            // (unbookmarked head), start one with empty bookmark_names.
            if current_segment.is_none() {
                current_segment = Some(BookmarkSegment {
                    bookmark_names: vec![],
                    change_id: change.change_id.clone(),
                    commits: Vec::new(),
                });
            }
            if let Some(ref mut seg) = current_segment {
                seg.commits.push(SegmentCommit {
                    commit_id: change.commit_id.clone(),
                    change_id: change.change_id.clone(),
                    description: change.description.clone(),
                    author: change.author.clone(),
                    committer: change.committer.clone(),
                    short_change_id: change.short_change_id.clone(),
                    files: vec![],
                    is_immutable: change.immutable,
                    local_bookmark_names: change.local_bookmark_names.clone(),
                    remote_bookmark_names: change.remote_bookmark_names.clone(),
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

/// Pre-fetch file lists for all commits in all stacks concurrently.
async fn fetch_file_lists<R: JjRunner>(
    jj: &Jj<R>,
    stacks: &mut [BranchStack],
) -> Result<(), StakkError> {
    // Collect all (stack_idx, seg_idx, commit_idx, commit_id) tuples.
    let mut tasks: Vec<(usize, usize, usize, String)> = Vec::new();
    for (si, stack) in stacks.iter().enumerate() {
        for (sgi, segment) in stack.segments.iter().enumerate() {
            for (ci, commit) in segment.commits.iter().enumerate() {
                if commit.files.is_empty() {
                    tasks.push((si, sgi, ci, commit.commit_id.clone()));
                }
            }
        }
    }

    let futures: Vec<_> = tasks
        .iter()
        .map(|(_, _, _, commit_id)| jj.get_diff_files(commit_id))
        .collect();

    let results = futures::future::join_all(futures).await;

    for ((si, sgi, ci, _), result) in tasks.iter().zip(results) {
        stacks[*si].segments[*sgi].commits[*ci].files = result?;
    }

    Ok(())
}

/// Walk from each leaf to root via the adjacency list, producing one
/// `BranchStack` per leaf. Each stack is ordered trunk-to-leaf (bottom first).
fn group_segments_into_stacks(
    stack_leaves: &HashSet<String>,
    adjacency_list: &HashMap<String, String>,
    segments: &HashMap<String, BookmarkSegment>,
) -> Vec<BranchStack> {
    let mut stacks = Vec::new();

    for leaf_id in stack_leaves {
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

    // Sort stacks by committer timestamps: collect each stack's commit
    // timestamps as instants in descending order, then sort stacks so the one
    // with the most recent instant comes first (leftmost column). Ties are
    // broken by the next-most-recent instant, etc. Final tiebreaker is the
    // leaf change_id for full determinism.
    stacks.sort_by(|a, b| {
        let ts_a = collect_timestamps_desc(a);
        let ts_b = collect_timestamps_desc(b);
        // Reverse comparison: largest (newest) timestamps first.
        ts_b.cmp(&ts_a).then_with(|| {
            fn leaf_change_id(s: &BranchStack) -> &str {
                s.segments
                    .last()
                    .map(|seg| seg.change_id.as_str())
                    .unwrap_or_default()
            }
            leaf_change_id(a).cmp(leaf_change_id(b))
        })
    });

    stacks
}

/// Collect all committer timestamps from a stack's commits as instants,
/// sorted descending.
///
/// Timestamps are parsed with jiff so that the UTC offset in the RFC 3339
/// string does not affect ordering: string comparison would place
/// `12:30:00+02:00` after `12:00:00+00:00` even though it is the earlier
/// instant. `graph::layout` reads the same field the same way, so the two
/// cannot disagree about which stack is *newer*. They are still not the same
/// order: layout ranks a sibling subtree by its maximum instant and tiebreaks
/// on the subtree root's change_id, while this ranks a stack by its whole
/// descending instant vector and tiebreaks on the leaf segment's — so JSON
/// `stacks[0]` and the TUI's leaf 1 are not guaranteed to be one stack.
///
/// Unparseable timestamps become `None`, which sorts as oldest — the same
/// tolerance `graph::layout` applies. jj always emits RFC 3339, so this is a
/// should-never-happen; failing here instead would make `stakk graph` error out
/// on input the TUI still renders, which is the divergence this parsing
/// exists to remove.
fn collect_timestamps_desc(stack: &BranchStack) -> Vec<Option<Timestamp>> {
    let mut timestamps: Vec<Option<Timestamp>> = stack
        .segments
        .iter()
        .flat_map(|seg| seg.commits.iter())
        .map(|c| c.committer.timestamp.parse().ok())
        .collect();
    timestamps.sort_unstable_by(|a, b| b.cmp(a));
    timestamps
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
        fn run_jj(
            &self,
            args: &[&str],
        ) -> impl std::future::Future<Output = Result<String, JjError>> + Send {
            std::future::ready((self.handler)(args))
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
        log_entry_json_full(commit_id, change_id, parents, local_bookmarks, false)
    }

    /// Build a log entry NDJSON line with an explicit `immutable` flag.
    fn log_entry_json_full(
        commit_id: &str,
        change_id: &str,
        parents: &[&str],
        local_bookmarks: &[&str],
        immutable: bool,
    ) -> String {
        let parents_json: Vec<String> = parents.iter().map(|p| format!("\"{p}\"")).collect();
        let parents_str = parents_json.join(",");

        let bookmarks_json: Vec<String> = local_bookmarks
            .iter()
            .map(|b| format!(r#"{{"name":"{b}","target":["{commit_id}"]}}"#))
            .collect();
        let bookmarks_str = bookmarks_json.join(",");

        let short = &change_id[..4.min(change_id.len())];
        format!(
            r#"{{"commit":{{"commit_id":"{commit_id}","parents":[{parents_str}],"change_id":"{change_id}","description":"desc {commit_id}","author":{{"name":"T","email":"t@t.t","timestamp":"T"}},"committer":{{"name":"T","email":"t@t.t","timestamp":"T"}}}},"local_bookmarks":[{bookmarks_str}],"remote_bookmarks":[],"immutable":{immutable},"short_change_id":"{short}"}}"#,
        )
    }

    // -- Tests --

    /// Simple linear stack: trunk -> `bm_a` -> `bm_b`
    ///
    /// Bookmark list returns [`bm_b`, `bm_a`].
    /// Traversing `bm_b`: log returns [`c_b(bm_b)`, `c_a(bm_a)`].
    /// `bm_a` is already discovered, so traversing `bm_a` is skipped.
    /// Result: 1 stack with 2 segments [`bm_a`, `bm_b`] (trunk-to-leaf).
    #[tokio::test]
    async fn linear_stack() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.segments.len(), 2);
        assert_eq!(graph.stacks.len(), 1);
        assert_eq!(graph.stack_leaves.len(), 1);
        assert!(graph.stack_leaves.contains("ch_b"));

        // Adjacency: ch_b -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_b").unwrap(), "ch_a");

        // Stack order is trunk-to-leaf: [bm_a, bm_b]
        let stack = &graph.stacks[0];
        assert_eq!(stack.segments.len(), 2);
        assert_eq!(stack.segments[0].bookmark_names, vec!["bm_a"]);
        assert_eq!(stack.segments[1].bookmark_names, vec!["bm_b"]);
    }

    /// Branching: trunk -> `bm_a` -> `bm_b` and trunk -> `bm_a` -> `bm_c`
    ///
    /// Two stacks sharing a common root (`bm_a`).
    /// `bm_b` and `bm_c` are both leaves.
    #[tokio::test]
    async fn branching_shared_root() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.segments.len(), 3);
        assert_eq!(graph.stacks.len(), 2);

        // Both bm_b and bm_c are leaves.
        assert!(graph.stack_leaves.contains("ch_b"));
        assert!(graph.stack_leaves.contains("ch_c"));

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
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.stacks.len(), 0);
        assert_eq!(graph.excluded_bookmarks, vec!["bm_merge"]);
        assert!(graph.tainted_change_ids.contains("ch_merge"));
    }

    /// Taint propagation: a descendant of a merge commit is also tainted.
    ///
    /// trunk -> `bm_a` (merge) -> `bm_b`
    /// When we traverse `bm_b` first, we find `bm_b`, then `bm_a` which is a
    /// merge. Both get tainted.
    #[tokio::test]
    async fn merge_taint_propagation() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.stacks.len(), 0);
        // bm_b excluded because its traversal hit a merge.
        assert_eq!(graph.excluded_bookmarks, vec!["bm_b"]);
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
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.stacks.len(), 0);
        assert_eq!(graph.excluded_bookmarks, vec!["bm_merge", "bm_child"]);
        assert!(graph.tainted_change_ids.contains("ch_merge"));
        assert!(graph.tainted_change_ids.contains("ch_child"));
    }

    /// Multiple bookmarks on the same change: single segment with both names.
    #[tokio::test]
    async fn multiple_bookmarks_same_change() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

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
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(String::new());
                }
                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert!(graph.segments.is_empty());
        assert!(graph.stacks.is_empty());
        assert!(graph.stack_leaves.is_empty());
        assert!(graph.excluded_bookmarks.is_empty());
        assert_eq!(graph.excluded_head_count, 0);
    }

    /// Multi-commit segment: unbookmarked commits between bookmarks are
    /// included in the parent-ward segment.
    ///
    /// trunk -> c1 -> `c2(bm_a)` -> c3 -> `c4(bm_b)`
    ///
    /// Segment `bm_b` should contain [c4, c3] (newest first).
    /// Segment `bm_a` should contain [c2, c1].
    #[tokio::test]
    async fn multi_commit_segment() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

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
    /// Bookmarks [`bm_b`, `bm_c`, `bm_a`] where:
    ///   trunk -> `bm_a` -> `bm_b`
    ///   trunk -> `bm_a` -> `bm_c`
    ///
    /// Traversing `bm_b` discovers [`bm_b`, `bm_a`].
    /// Traversing `bm_c` discovers [`bm_c`], stops at `bm_a` (already
    /// collected). `bm_a` is NOT traversed separately (already collected).
    #[tokio::test]
    async fn already_collected_early_stop() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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

                // Heads query: no unbookmarked heads in this test.
                if is_heads_query(args) {
                    return Ok(String::new());
                }

                // Should NOT be called for c_a because bm_a is already
                // collected.
                panic!("unexpected revset: {revset}");
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // bm_a segment is NOT duplicated.
        assert_eq!(graph.segments.len(), 3);
        assert_eq!(graph.stacks.len(), 2);

        // Adjacency: ch_b -> ch_a, ch_c -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_b").unwrap(), "ch_a");
        assert_eq!(graph.adjacency_list.get("ch_c").unwrap(), "ch_a");
    }

    /// Single bookmark, single commit — simplest possible case.
    #[tokio::test]
    async fn single_bookmark_single_commit() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);
        assert!(graph.stack_leaves.contains("ch_x"));
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
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        let seg = graph.segments.get("ch1").unwrap();
        assert_eq!(seg.commits[0].commit_id, "c1");
        assert_eq!(seg.commits[0].change_id, "ch1");
        assert_eq!(seg.commits[0].description, "desc c1");
        assert_eq!(seg.commits[0].author.name, "T");
    }

    /// `group_segments_into_stacks` orders by most-recent committer timestamp
    /// first (leftmost).
    #[test]
    fn stacks_are_ordered_by_timestamp() {
        use crate::jj::types::Signature;

        let mut segments = HashMap::new();
        let adjacency_list = HashMap::new();
        let mut stack_leaves = HashSet::new();

        let test_sig = |ts: &str| Signature {
            name: "T".to_string(),
            email: "t@t.t".to_string(),
            timestamp: ts.to_string(),
        };

        // Three independent leaves with different timestamps.
        // z_leaf has the newest timestamp, a_leaf the oldest.
        for (id, ts) in [
            ("z_leaf", "2026-03-01T00:00:00Z"),
            ("a_leaf", "2026-03-03T00:00:00Z"),
            ("m_leaf", "2026-03-02T00:00:00Z"),
        ] {
            segments.insert(
                id.to_string(),
                BookmarkSegment {
                    bookmark_names: vec![id.to_string()],
                    change_id: id.to_string(),
                    commits: vec![SegmentCommit {
                        commit_id: format!("c_{id}"),
                        change_id: id.to_string(),
                        description: String::new(),
                        author: test_sig(ts),
                        committer: test_sig(ts),
                        short_change_id: id[..4].to_string(),
                        files: vec![],
                        is_immutable: false,
                        local_bookmark_names: vec![],
                        remote_bookmark_names: vec![],
                    }],
                },
            );
            stack_leaves.insert(id.to_string());
        }

        let stacks = group_segments_into_stacks(&stack_leaves, &adjacency_list, &segments);

        assert_eq!(stacks.len(), 3);
        // Sorted by newest committer timestamp first.
        assert_eq!(stacks[0].segments[0].change_id, "a_leaf"); // 2026-03-03
        assert_eq!(stacks[1].segments[0].change_id, "m_leaf"); // 2026-03-02
        assert_eq!(stacks[2].segments[0].change_id, "z_leaf"); // 2026-03-01
    }

    /// Stack order compares committer timestamps as instants, not as strings,
    /// so a UTC offset cannot flip it. Mirrors
    /// `layout::tests::sibling_order_uses_offset_aware_timestamps`.
    #[test]
    fn stacks_ordered_by_offset_aware_timestamps() {
        use crate::jj::types::Signature;

        let mut segments = HashMap::new();
        let adjacency_list = HashMap::new();
        let mut stack_leaves = HashSet::new();

        let test_sig = |ts: &str| Signature {
            name: "T".to_string(),
            email: "t@t.t".to_string(),
            timestamp: ts.to_string(),
        };

        // 12:00+00:00 is the *later* instant than 12:30+02:00 (= 10:30 UTC),
        // but lexicographic string comparison would say otherwise.
        for (id, ts) in [
            ("utc", "2026-01-01T12:00:00+00:00"),
            ("offset", "2026-01-01T12:30:00+02:00"),
        ] {
            segments.insert(
                id.to_string(),
                BookmarkSegment {
                    bookmark_names: vec![id.to_string()],
                    change_id: id.to_string(),
                    commits: vec![SegmentCommit {
                        commit_id: format!("c_{id}"),
                        change_id: id.to_string(),
                        description: String::new(),
                        author: test_sig(ts),
                        committer: test_sig(ts),
                        short_change_id: id[..3].to_string(),
                        files: vec![],
                        is_immutable: false,
                        local_bookmark_names: vec![],
                        remote_bookmark_names: vec![],
                    }],
                },
            );
            stack_leaves.insert(id.to_string());
        }

        let stacks = group_segments_into_stacks(&stack_leaves, &adjacency_list, &segments);

        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[0].segments[0].change_id, "utc");
        assert_eq!(stacks[1].segments[0].change_id, "offset");
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
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.segments.len(), 1);
        let seg = graph.segments.get("ch_x").unwrap();
        assert_eq!(seg.bookmark_names, vec!["bm_user"]);
    }

    /// The immutable flag and unfiltered local bookmark names are threaded
    /// into `SegmentCommit`, even for bookmarks excluded from the graph.
    ///
    /// Commit c_mid is immutable and carries bookmark bm_pinned, which the
    /// bookmarks revset filtered out (not in `get_my_bookmarks` output). It
    /// must not become a segment boundary, but its commit must record the
    /// flag and the bookmark name so later phases can diagnose the exclusion.
    #[tokio::test]
    async fn immutable_flag_and_local_bookmarks_threaded() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_leaf", "c_leaf", "ch_leaf"));
                }

                let revset = args[2];
                if revset.contains("c_leaf") {
                    let lines = [
                        log_entry_json("c_leaf", "ch_leaf", &["c_mid"], &["bm_leaf"]),
                        log_entry_json_full("c_mid", "ch_mid", &["trunk_c"], &["bm_pinned"], true),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // bm_pinned is not a boundary: one segment, named after bm_leaf only.
        assert_eq!(graph.segments.len(), 1);
        let seg = graph.segments.get("ch_leaf").unwrap();
        assert_eq!(seg.bookmark_names, vec!["bm_leaf"]);
        assert_eq!(seg.commits.len(), 2);

        let leaf_commit = &seg.commits[0];
        assert!(!leaf_commit.is_immutable);
        assert_eq!(leaf_commit.local_bookmark_names, vec!["bm_leaf"]);

        let mid_commit = &seg.commits[1];
        assert!(mid_commit.is_immutable);
        assert_eq!(mid_commit.local_bookmark_names, vec!["bm_pinned"]);
    }

    /// A commit with only non-user bookmarks is treated as unbookmarked
    /// (no segment boundary).
    ///
    /// trunk -> `c_other(bm_other)` -> `c_user(bm_user)`
    /// Only `bm_user` is the user's bookmark. `c_other` has only `bm_other`, so
    /// it should be treated as an unbookmarked commit within `bm_user`'s
    /// segment.
    #[tokio::test]
    async fn only_non_user_bookmarks_no_segment_boundary() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
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
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // Only one segment (bm_user), containing both commits.
        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);

        let seg = graph.segments.get("ch_user").unwrap();
        assert_eq!(seg.bookmark_names, vec!["bm_user"]);
        assert_eq!(seg.commits.len(), 2);
        assert_eq!(seg.commits[0].commit_id, "c_user");
        assert_eq!(seg.commits[1].commit_id, "c_other");
    }

    // -- Unbookmarked head tests --

    /// Helper: determines if a `jj log` invocation is a heads query vs
    /// traversal. Heads queries contain `"heads("` in the revset.
    fn is_heads_query(args: &[&str]) -> bool {
        args[0] == "log" && args[2].contains("heads(")
    }

    /// trunk → `bm_a` → `change_1` (no bookmark)
    ///
    /// Head at `change_1` creates a 2-segment stack: the unbookmarked head
    /// segment plus the bookmarked `bm_a` segment discovered during traversal.
    #[tokio::test]
    async fn unbookmarked_head_discovered() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_a", "c_a", "ch_a"));
                }

                if is_heads_query(args) {
                    // Head is at c_h (beyond bm_a).
                    return Ok(log_entry_json("c_h", "ch_h", &["c_a"], &[]));
                }

                // Traversal queries.
                let revset = args[2];
                if revset.contains("c_a") {
                    return Ok(log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]));
                }
                if revset.contains("c_h") {
                    let lines = [
                        log_entry_json("c_h", "ch_h", &["c_a"], &[]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // Two segments: bm_a (bookmarked) and ch_h (unbookmarked head).
        assert_eq!(graph.segments.len(), 2);
        assert!(graph.segments.contains_key("ch_a"));
        assert!(graph.segments.contains_key("ch_h"));

        // The unbookmarked segment has empty bookmark_names.
        let head_seg = graph.segments.get("ch_h").unwrap();
        assert!(head_seg.bookmark_names.is_empty());
        assert_eq!(head_seg.commits.len(), 1);
        assert_eq!(head_seg.commits[0].commit_id, "c_h");

        // Adjacency: ch_h -> ch_a
        assert_eq!(graph.adjacency_list.get("ch_h").unwrap(), "ch_a");

        // One stack with 2 segments.
        assert_eq!(graph.stacks.len(), 1);
        let stack = &graph.stacks[0];
        assert_eq!(stack.segments.len(), 2);
        assert_eq!(stack.segments[0].change_id, "ch_a");
        assert_eq!(stack.segments[1].change_id, "ch_h");
    }

    /// Head at the same commit as a bookmark — should be skipped (no
    /// duplicate segment).
    #[tokio::test]
    async fn unbookmarked_head_at_bookmarked_commit_skipped() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_a", "c_a", "ch_a"));
                }

                if is_heads_query(args) {
                    // Head is at the same commit as bm_a.
                    return Ok(log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]));
                }

                let revset = args[2];
                if revset.contains("c_a") {
                    return Ok(log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // Only one segment — the head at bm_a's commit was skipped.
        assert_eq!(graph.segments.len(), 1);
        assert_eq!(graph.stacks.len(), 1);
        assert!(graph.segments.contains_key("ch_a"));
    }

    /// Two unbookmarked heads branching from a bookmarked ancestor.
    ///
    /// trunk → `bm_a` → `head_1` (no bm)
    ///       ↘ `bm_a` → `head_2` (no bm)
    #[tokio::test]
    async fn multiple_unbookmarked_heads() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_a", "c_a", "ch_a"));
                }

                if is_heads_query(args) {
                    let lines = [
                        log_entry_json("c_h1", "ch_h1", &["c_a"], &[]),
                        log_entry_json("c_h2", "ch_h2", &["c_a"], &[]),
                    ];
                    return Ok(lines.join("\n"));
                }

                let revset = args[2];
                if revset.contains("c_a") {
                    return Ok(log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]));
                }
                if revset.contains("c_h1") {
                    let lines = [
                        log_entry_json("c_h1", "ch_h1", &["c_a"], &[]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }
                if revset.contains("c_h2") {
                    let lines = [
                        log_entry_json("c_h2", "ch_h2", &["c_a"], &[]),
                        log_entry_json("c_a", "ch_a", &["trunk_c"], &["bm_a"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        // Three segments: bm_a, ch_h1, ch_h2.
        assert_eq!(graph.segments.len(), 3);
        assert_eq!(graph.stacks.len(), 2);

        // Both heads connect to bm_a.
        assert_eq!(graph.adjacency_list.get("ch_h1").unwrap(), "ch_a");
        assert_eq!(graph.adjacency_list.get("ch_h2").unwrap(), "ch_a");

        // Both head segments have empty bookmark names.
        assert!(
            graph
                .segments
                .get("ch_h1")
                .unwrap()
                .bookmark_names
                .is_empty()
        );
        assert!(
            graph
                .segments
                .get("ch_h2")
                .unwrap()
                .bookmark_names
                .is_empty()
        );
    }

    /// Unbookmarked head with a bookmarked ancestor — traversal from the
    /// unbookmarked head discovers the bookmark during the walk and creates
    /// proper boundary.
    ///
    /// trunk → `c_mid(bm_mid)` → `c_head` (no bm)
    /// No bookmark is at `c_head`. `bm_mid` is the only bookmark.
    #[tokio::test]
    async fn unbookmarked_head_with_bookmarked_ancestor() {
        let runner = MockJjRunner {
            handler: |args: &[&str]| {
                if args[0] == "diff" {
                    return Ok(String::new());
                }
                if args[0] == "bookmark" {
                    return Ok(bookmark_json("bm_mid", "c_mid", "ch_mid"));
                }

                if is_heads_query(args) {
                    return Ok(log_entry_json("c_head", "ch_head", &["c_mid"], &[]));
                }

                let revset = args[2];
                if revset.contains("c_mid") {
                    return Ok(log_entry_json("c_mid", "ch_mid", &["trunk_c"], &["bm_mid"]));
                }
                if revset.contains("c_head") {
                    let lines = [
                        log_entry_json("c_head", "ch_head", &["c_mid"], &[]),
                        log_entry_json("c_mid", "ch_mid", &["trunk_c"], &["bm_mid"]),
                    ];
                    return Ok(lines.join("\n"));
                }

                Ok(String::new())
            },
        };

        let jj = Jj::new(runner);
        let graph = build_change_graph(
            &jj,
            "mine() ~ trunk() ~ immutable()",
            "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        )
        .await
        .unwrap();

        assert_eq!(graph.segments.len(), 2);
        assert_eq!(graph.stacks.len(), 1);

        // ch_head segment has no bookmarks.
        let head_seg = graph.segments.get("ch_head").unwrap();
        assert!(head_seg.bookmark_names.is_empty());

        // ch_mid segment has the bookmark.
        let mid_seg = graph.segments.get("ch_mid").unwrap();
        assert_eq!(mid_seg.bookmark_names, vec!["bm_mid"]);

        // Adjacency: ch_head -> ch_mid.
        assert_eq!(graph.adjacency_list.get("ch_head").unwrap(), "ch_mid");

        // Stack: [bm_mid, head].
        let stack = &graph.stacks[0];
        assert_eq!(stack.segments.len(), 2);
        assert_eq!(stack.segments[0].change_id, "ch_mid");
        assert_eq!(stack.segments[1].change_id, "ch_head");
    }

    // -- push state --

    fn bookmark(name: &str, synced: bool) -> Bookmark {
        Bookmark {
            name: name.to_string(),
            commit_id: format!("c_{name}"),
            change_id: format!("ch_{name}"),
            synced,
        }
    }

    /// One stack, one segment, one bookmark, whose boundary commit carries
    /// `remotes`.
    fn stack_with(name: &str, remotes: &[&str]) -> BranchStack {
        BranchStack {
            segments: vec![BookmarkSegment {
                bookmark_names: vec![name.to_string()],
                change_id: format!("ch_{name}"),
                commits: vec![SegmentCommit {
                    commit_id: format!("c_{name}"),
                    change_id: format!("ch_{name}"),
                    description: String::new(),
                    author: test_signature(),
                    committer: test_signature(),
                    short_change_id: "ch".to_string(),
                    files: vec![],
                    is_immutable: false,
                    local_bookmark_names: vec![name.to_string()],
                    remote_bookmark_names: remotes.iter().map(ToString::to_string).collect(),
                }],
            }],
        }
    }

    fn test_signature() -> crate::jj::types::Signature {
        crate::jj::types::Signature {
            name: "T".to_string(),
            email: "t@t.t".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// A never-pushed bookmark and an up-to-date one are both `synced` as far
    /// as jj is concerned. Only the remote ref on the boundary commit tells
    /// them apart, and getting this backwards would report every new stack as
    /// already pushed.
    #[test]
    fn synced_alone_does_not_separate_unpushed_from_synced() {
        let states = derive_remote_states(
            &[bookmark("fresh", true), bookmark("live", true)],
            &[
                stack_with("fresh", &[]),
                stack_with("live", &["live@origin"]),
            ],
        );

        assert_eq!(states["fresh"], RemoteState::Unpushed);
        assert_eq!(states["live"], RemoteState::Synced);
    }

    /// After a rebase the remote ref stays on the old commit, so it is absent
    /// from the boundary commit — `synced == false` is what catches it.
    #[test]
    fn an_out_of_date_remote_is_diverged() {
        let states = derive_remote_states(&[bookmark("moved", false)], &[stack_with("moved", &[])]);

        assert_eq!(states["moved"], RemoteState::Diverged);
    }

    /// jj's internal `@git` remote is not a push target, so a bookmark that
    /// only exists there has never been pushed anywhere real.
    #[test]
    fn the_internal_git_remote_does_not_count_as_pushed() {
        let states = derive_remote_states(
            &[bookmark("local", true)],
            &[stack_with("local", &["local@git"])],
        );

        assert_eq!(states["local"], RemoteState::Unpushed);
    }

    /// A remote bookmark whose name merely starts with ours is a different
    /// bookmark; matching on a bare prefix would call `feat` synced because
    /// `feat-2@origin` is on the commit.
    #[test]
    fn a_longer_bookmark_name_is_not_a_match() {
        let states = derive_remote_states(
            &[bookmark("feat", true)],
            &[stack_with("feat", &["feat-2@origin"])],
        );

        assert_eq!(states["feat"], RemoteState::Unpushed);
    }

    /// The unbookmarked head segment contributes no entry rather than an
    /// empty-named one.
    #[test]
    fn an_unbookmarked_segment_contributes_nothing() {
        let mut stack = stack_with("head", &[]);
        stack.segments[0].bookmark_names.clear();

        assert!(derive_remote_states(&[], &[stack]).is_empty());
    }
}
