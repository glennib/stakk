//! Convert a `ChangeGraph` into a jj-log-style row layout for rendering.
//!
//! Commits are deduplicated by `commit_id` into a tree rooted at a trunk
//! pseudo-node, then flattened into display rows: one row per commit plus
//! connector rows (`├─╯`) where a sibling subtree merges into its parent's
//! column. Sibling subtrees are ordered newest-first by committer timestamp,
//! like jj log.

use std::collections::HashMap;

use jiff::Timestamp;

use crate::graph::types::ChangeGraph;
use crate::jj::types::Signature;

/// Glyphs shared by all renderers of the layout (the TUI graph widget and
/// `stakk show`'s pretty format).
///
/// Node glyph for a commit on the selected path (TUI only).
pub const NODE_SELECTED: &str = "\u{25cf}"; // ●
/// Node glyph for a commit.
pub const NODE_OTHER: &str = "\u{25cb}"; // ○
/// Node glyph for the trunk pseudo-node.
pub const TRUNK_CHAR: &str = "\u{25c6}"; // ◆
/// Marker for a collapsed run of commits (TUI only).
pub const ELLIPSIS: &str = "\u{22ef}"; // ⋯
/// One edge-gutter cell.
pub const GUTTER_CELL: &str = "\u{2502} "; // "│ "
/// First cell of a connector row.
pub const CONNECTOR_TEE: &str = "\u{251c}"; // ├
/// Tail of a connector row.
pub const CONNECTOR_TAIL: &str = "\u{2500}\u{256f}"; // ─╯
/// Marker trailing the selected leaf (TUI only).
pub const LEAF_MARKER: &str = "\u{25c0}"; // ◀

/// A commit node in the layout tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutNode {
    /// The jj change ID for this commit.
    pub change_id: String,
    /// The jj commit ID.
    pub commit_id: String,
    /// First line of the commit description.
    pub summary: String,
    /// Full commit description.
    pub description: String,
    /// Segment-boundary bookmark names on this commit (may be empty). Only
    /// the last commit of a segment carries them — not to be conflated with
    /// `excluded_bookmarks`.
    pub bookmark_names: Vec<String>,
    /// Local bookmark names on this commit that are *not* segment-boundary
    /// names — bookmarks the graph's bookmarks revset filtered out (e.g. on
    /// immutable commits).
    pub excluded_bookmarks: Vec<String>,
    /// Whether jj considers this commit immutable.
    pub is_immutable: bool,
    /// Whether this node is the trunk node.
    pub is_trunk: bool,
    /// Whether this node is a leaf (no children).
    pub is_leaf: bool,
    /// Shortest unique change ID prefix (from jj).
    pub short_change_id: String,
    /// Author signature.
    pub author: Signature,
    /// Files changed by this commit.
    pub files: Vec<String>,
    /// Index of the parent node (toward trunk); `None` for the trunk node.
    pub parent: Option<usize>,
}

/// One display row of the graph, in top-to-bottom order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRow {
    /// A commit (or trunk) row: node index into `GraphLayout::nodes`, drawn
    /// at the given gutter column.
    Commit { node: usize, col: usize },
    /// A connector row (`├─╯`) merging column `col` into `col - 1`.
    Connector { col: usize },
}

/// The complete jj-log-style layout of the change graph.
#[derive(Debug, Clone)]
pub struct GraphLayout {
    /// All nodes. Index 0 is the trunk pseudo-node (when non-empty).
    pub nodes: Vec<LayoutNode>,
    /// Display rows, top to bottom (trunk last).
    pub rows: Vec<GraphRow>,
    /// Node indices of leaves, in display order (top to bottom).
    pub leaves: Vec<usize>,
}

impl GraphLayout {
    /// Return all leaf nodes (selectable branch tips) in display order (top
    /// to bottom), so that index 0 is the topmost leaf.
    pub fn leaf_nodes(&self) -> Vec<&LayoutNode> {
        self.leaves.iter().map(|&i| &self.nodes[i]).collect()
    }

    /// Node indices on the path from trunk to the given node, trunk first.
    pub fn path_node_indices(&self, node: usize) -> Vec<usize> {
        let mut path = Vec::new();
        let mut current = Some(node);
        while let Some(i) = current {
            path.push(i);
            current = self.nodes[i].parent;
        }
        path.reverse();
        path
    }

    /// Collect all nodes on the path from trunk to the given node.
    ///
    /// Returns nodes in trunk-to-leaf order.
    pub fn path_to_leaf(&self, node: usize) -> Vec<&LayoutNode> {
        self.path_node_indices(node)
            .into_iter()
            .map(|i| &self.nodes[i])
            .collect()
    }

    /// Display row index of the commit row for a node.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in tests; useful layout diagnostic")
    )]
    pub fn row_of_node(&self, node: usize) -> Option<usize> {
        self.rows.iter().position(|r| match r {
            GraphRow::Commit { node: n, .. } => *n == node,
            GraphRow::Connector { .. } => false,
        })
    }
}

/// Build a jj-log-style graph layout from a `ChangeGraph`.
///
/// The trunk pseudo-node sits at the bottom; each commit appears exactly
/// once. Sibling subtrees are ordered by their most recent committer
/// timestamp, newest topmost, with the subtree root's `change_id` as a
/// deterministic tiebreaker.
pub fn build_layout(graph: &ChangeGraph) -> GraphLayout {
    if graph.stacks.is_empty() {
        return GraphLayout {
            nodes: vec![],
            rows: vec![],
            leaves: vec![],
        };
    }

    let mut nodes: Vec<LayoutNode> = vec![LayoutNode {
        change_id: String::new(),
        commit_id: String::new(),
        summary: "trunk".to_string(),
        description: String::new(),
        bookmark_names: vec![],
        excluded_bookmarks: vec![],
        is_immutable: false,
        is_trunk: true,
        is_leaf: false,
        short_change_id: String::new(),
        author: Signature {
            name: String::new(),
            email: String::new(),
            timestamp: String::new(),
        },
        files: vec![],
        parent: None,
    }];
    let mut children: Vec<Vec<usize>> = vec![vec![]];
    // Committer timestamp per node; unparseable timestamps sort as oldest.
    let mut own_ts: Vec<Option<Timestamp>> = vec![None];
    // commit_id → node index, for deduplicating shared segments across
    // stacks (change_id is shared by all commits in a segment, so key by
    // commit_id).
    let mut placed: HashMap<String, usize> = HashMap::new();

    for stack in &graph.stacks {
        let mut prev: usize = 0; // trunk

        for (seg_idx, segment) in stack.segments.iter().enumerate() {
            let is_last_segment = seg_idx == stack.segments.len() - 1;

            // Commits are newest-first in the segment; walk them
            // trunk-to-leaf.
            let commits: Vec<_> = segment.commits.iter().rev().collect();

            for (commit_idx, commit) in commits.iter().enumerate() {
                // Shared node (prefix of an earlier stack): continue from it.
                if let Some(&existing) = placed.get(&commit.commit_id) {
                    prev = existing;
                    continue;
                }

                let is_last_commit_in_segment = commit_idx == commits.len() - 1;

                let bookmark_names = if is_last_commit_in_segment {
                    segment.bookmark_names.clone()
                } else {
                    vec![]
                };

                let excluded_bookmarks: Vec<String> = commit
                    .local_bookmark_names
                    .iter()
                    .filter(|name| !bookmark_names.contains(name))
                    .cloned()
                    .collect();

                let summary = commit
                    .description
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .unwrap_or("(no description)")
                    .to_string();

                let idx = nodes.len();
                nodes.push(LayoutNode {
                    change_id: commit.change_id.clone(),
                    commit_id: commit.commit_id.clone(),
                    summary,
                    description: commit.description.clone(),
                    bookmark_names,
                    excluded_bookmarks,
                    is_immutable: commit.is_immutable,
                    is_trunk: false,
                    is_leaf: is_last_segment && is_last_commit_in_segment,
                    short_change_id: commit.short_change_id.clone(),
                    author: commit.author.clone(),
                    files: commit.files.clone(),
                    parent: Some(prev),
                });
                children.push(vec![]);
                own_ts.push(commit.committer.timestamp.parse().ok());
                children[prev].push(idx);
                placed.insert(commit.commit_id.clone(), idx);
                prev = idx;
            }
        }
    }

    // Max committer timestamp per subtree. Parents are always created before
    // their children, so a reverse index scan sees children first.
    let mut subtree_ts = own_ts;
    for i in (0..nodes.len()).rev() {
        for &child in &children[i] {
            if subtree_ts[child] > subtree_ts[i] {
                subtree_ts[i] = subtree_ts[child];
            }
        }
    }

    // Order siblings newest-subtree-first, tiebreaking on the subtree root's
    // change_id for determinism.
    for siblings in &mut children {
        siblings.sort_by(|&a, &b| {
            subtree_ts[b]
                .cmp(&subtree_ts[a])
                .then_with(|| nodes[a].change_id.cmp(&nodes[b].change_id))
        });
    }

    let rows = build_rows(&children);

    let leaves: Vec<usize> = rows
        .iter()
        .filter_map(|row| match row {
            GraphRow::Commit { node, .. } if nodes[*node].is_leaf => Some(*node),
            _ => None,
        })
        .collect();

    GraphLayout {
        nodes,
        rows,
        leaves,
    }
}

/// Flatten the tree into display rows, top to bottom.
///
/// For a node at column `col`, its first (newest) child subtree continues at
/// `col` directly above it; each further sibling subtree renders at
/// `col + 1`, followed by a connector row merging back into `col`. The
/// node's own row comes last (lowest).
fn build_rows(children: &[Vec<usize>]) -> Vec<GraphRow> {
    enum Work {
        Visit { node: usize, col: usize },
        EmitCommit { node: usize, col: usize },
        EmitConnector { col: usize },
    }

    let mut rows = Vec::new();
    let mut work = vec![Work::Visit { node: 0, col: 0 }];

    while let Some(item) = work.pop() {
        match item {
            Work::Visit { node, col } => {
                // Emission order: first child's subtree, then each further
                // sibling's subtree followed by its connector, then this
                // node's own row. Push in reverse (LIFO).
                work.push(Work::EmitCommit { node, col });
                for &sibling in children[node].iter().skip(1).rev() {
                    work.push(Work::EmitConnector { col: col + 1 });
                    work.push(Work::Visit {
                        node: sibling,
                        col: col + 1,
                    });
                }
                if let Some(&first) = children[node].first() {
                    work.push(Work::Visit { node: first, col });
                }
            }
            Work::EmitCommit { node, col } => rows.push(GraphRow::Commit { node, col }),
            Work::EmitConnector { col } => rows.push(GraphRow::Connector { col }),
        }
    }

    rows
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use super::*;
    use crate::graph::types::BookmarkSegment;
    use crate::graph::types::BranchStack;
    use crate::graph::types::ChangeGraph;
    use crate::graph::types::SegmentCommit;

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: HashSet::new(),
            stack_roots: HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: HashSet::new(),
            excluded_bookmark_count: 0,
            stacks,
        }
    }

    fn make_segment(names: &[&str], change_id: &str, descriptions: &[&str]) -> BookmarkSegment {
        make_segment_at(names, change_id, descriptions, "T")
    }

    /// Like `make_segment`, with an explicit committer timestamp for
    /// ordering tests. The default "T" is unparseable and sorts as oldest.
    fn make_segment_at(
        names: &[&str],
        change_id: &str,
        descriptions: &[&str],
        timestamp: &str,
    ) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(ToString::to_string).collect(),
            change_id: change_id.to_string(),
            commits: descriptions
                .iter()
                .enumerate()
                .map(|(i, desc)| SegmentCommit {
                    commit_id: format!("c_{change_id}_{i}"),
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
                        timestamp: timestamp.to_string(),
                    },
                    files: vec![],
                    short_change_id: change_id[..4.min(change_id.len())].to_string(),
                    is_immutable: false,
                    local_bookmark_names: vec![],
                })
                .collect(),
        }
    }

    /// Render the display rows as plain text for structural assertions:
    /// `col:summary` for commit rows, `col:├─╯` for connectors.
    fn rows_to_string(layout: &GraphLayout) -> String {
        layout
            .rows
            .iter()
            .map(|row| match row {
                GraphRow::Commit { node, col } => {
                    format!("{col}:{}", layout.nodes[*node].summary)
                }
                GraphRow::Connector { col } => format!("{col}:├─╯"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_graph_layout() {
        let graph = make_graph(vec![]);
        let layout = build_layout(&graph);
        assert!(layout.nodes.is_empty());
        assert!(layout.rows.is_empty());
        assert!(layout.leaves.is_empty());
    }

    #[test]
    fn single_linear_stack() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["add base"]),
                make_segment(&["leaf"], "ch_b", &["add leaf"]),
            ],
        }]);

        let layout = build_layout(&graph);

        // trunk + 2 commits = 3 nodes
        assert_eq!(layout.nodes.len(), 3);
        assert!(layout.nodes[0].is_trunk);

        let base = &layout.nodes[1];
        assert_eq!(base.bookmark_names, vec!["base"]);
        assert!(!base.is_leaf);
        assert_eq!(base.parent, Some(0));

        let leaf = &layout.nodes[2];
        assert_eq!(leaf.bookmark_names, vec!["leaf"]);
        assert!(leaf.is_leaf);
        assert_eq!(leaf.parent, Some(1));

        // Display order: leaf on top, trunk at the bottom, all at column 0.
        assert_eq!(rows_to_string(&layout), "0:add leaf\n0:add base\n0:trunk");
    }

    #[test]
    fn two_branching_stacks_ordered_newest_first() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-01-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
        ]);

        let layout = build_layout(&graph);

        // trunk + 2 commits = 3 nodes; beta is newer so it renders topmost
        // at column 0, alpha below it at column 1 with a connector.
        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(
            rows_to_string(&layout),
            "0:beta work\n1:alpha work\n1:├─╯\n0:trunk"
        );

        // Leaves in display order: beta first (topmost).
        let leaves = layout.leaf_nodes();
        assert_eq!(leaves[0].bookmark_names, vec!["beta"]);
        assert_eq!(leaves[1].bookmark_names, vec!["alpha"]);
    }

    #[test]
    fn sibling_order_uses_offset_aware_timestamps() {
        // 12:00+00:00 is *later* than 12:30+02:00 (= 10:30 UTC), but
        // lexicographic string comparison would say otherwise.
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["utc"],
                    "ch_utc",
                    &["utc work"],
                    "2026-01-01T12:00:00+00:00",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["offset"],
                    "ch_offset",
                    &["offset work"],
                    "2026-01-01T12:30:00+02:00",
                )],
            },
        ]);

        let layout = build_layout(&graph);
        let leaves = layout.leaf_nodes();
        assert_eq!(leaves[0].bookmark_names, vec!["utc"]);
        assert_eq!(leaves[1].bookmark_names, vec!["offset"]);
    }

    #[test]
    fn sibling_order_tiebreaks_on_change_id() {
        // Identical (unparseable) timestamps: order falls back to change_id.
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment(&["zeta"], "ch_z", &["z work"])],
            },
            BranchStack {
                segments: vec![make_segment(&["alpha"], "ch_a", &["a work"])],
            },
        ]);

        let layout = build_layout(&graph);
        let leaves = layout.leaf_nodes();
        assert_eq!(leaves[0].bookmark_names, vec!["alpha"]);
        assert_eq!(leaves[1].bookmark_names, vec!["zeta"]);
    }

    #[test]
    fn shared_root_segment() {
        // Two stacks sharing a root segment (same change_id).
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared base"]),
                    make_segment_at(&["feat-a"], "ch_a", &["feature a"], "2026-02-01T00:00:00Z"),
                ],
            },
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared base"]),
                    make_segment_at(&["feat-b"], "ch_b", &["feature b"], "2026-01-01T00:00:00Z"),
                ],
            },
        ]);

        let layout = build_layout(&graph);

        // trunk + shared base + feat-a + feat-b = 4 nodes; the shared base
        // appears exactly once.
        assert_eq!(layout.nodes.len(), 4);
        let shared: Vec<_> = layout
            .nodes
            .iter()
            .filter(|n| n.change_id == "ch_shared")
            .collect();
        assert_eq!(shared.len(), 1);

        // feat-a is newer: it continues in the shared base's column; feat-b
        // branches off at column 1.
        assert_eq!(
            rows_to_string(&layout),
            "0:feature a\n1:feature b\n1:├─╯\n0:shared base\n0:trunk"
        );
    }

    #[test]
    fn multi_commit_segment() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(
                &["feat"],
                "ch_a",
                &["second commit", "first commit"],
            )],
        }]);

        let layout = build_layout(&graph);

        // trunk + 2 commits = 3 nodes; newest commit on top.
        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(
            rows_to_string(&layout),
            "0:second commit\n0:first commit\n0:trunk"
        );

        // Only the last commit in the segment carries the bookmark.
        let first = layout
            .nodes
            .iter()
            .find(|n| n.summary == "first commit")
            .unwrap();
        let second = layout
            .nodes
            .iter()
            .find(|n| n.summary == "second commit")
            .unwrap();
        assert!(first.bookmark_names.is_empty());
        assert_eq!(second.bookmark_names, vec!["feat"]);
    }

    #[test]
    fn path_to_leaf_linear() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["base work"]),
                make_segment(&["leaf"], "ch_b", &["leaf work"]),
            ],
        }]);

        let layout = build_layout(&graph);
        let path = layout.path_to_leaf(layout.leaves[0]);

        // trunk → base → leaf
        assert_eq!(path.len(), 3);
        assert!(path[0].is_trunk);
        assert_eq!(path[1].change_id, "ch_a");
        assert_eq!(path[2].change_id, "ch_b");
    }

    #[test]
    fn path_to_leaf_branching() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared"]),
                    make_segment(&["feat-a"], "ch_a", &["feature a"]),
                ],
            },
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared"]),
                    make_segment(&["feat-b"], "ch_b", &["feature b"]),
                ],
            },
        ]);

        let layout = build_layout(&graph);

        // Path to feat-b should go through the shared base.
        let feat_b = layout
            .nodes
            .iter()
            .position(|n| n.change_id == "ch_b")
            .unwrap();
        let path = layout.path_to_leaf(feat_b);

        // trunk → shared → feat_b
        assert_eq!(path.len(), 3);
        assert!(path[0].is_trunk);
        assert_eq!(path[1].change_id, "ch_shared");
        assert_eq!(path[2].change_id, "ch_b");
    }

    #[test]
    fn immutable_flag_and_excluded_bookmarks_threaded() {
        // Two-commit segment: the mid-segment commit is immutable and carries
        // a bookmark that the graph filtered out; the boundary commit carries
        // its own boundary name plus a filtered-out extra.
        let mut segment = make_segment(&["feat"], "ch_a", &["second commit", "first commit"]);
        // Commits are newest-first: [0] = boundary ("second"), [1] = mid.
        segment.commits[0].local_bookmark_names =
            vec!["feat".to_string(), "other-user".to_string()];
        segment.commits[1].is_immutable = true;
        segment.commits[1].local_bookmark_names = vec!["pinned".to_string()];

        let graph = make_graph(vec![BranchStack {
            segments: vec![segment],
        }]);
        let layout = build_layout(&graph);

        let trunk = &layout.nodes[0];
        assert!(trunk.is_trunk);
        assert!(!trunk.is_immutable);
        assert!(trunk.excluded_bookmarks.is_empty());

        let mid = layout
            .nodes
            .iter()
            .find(|n| n.summary == "first commit")
            .unwrap();
        assert!(mid.is_immutable);
        assert!(mid.bookmark_names.is_empty());
        assert_eq!(mid.excluded_bookmarks, vec!["pinned"]);

        // Boundary names are subtracted from excluded_bookmarks.
        let boundary = layout
            .nodes
            .iter()
            .find(|n| n.summary == "second commit")
            .unwrap();
        assert!(!boundary.is_immutable);
        assert_eq!(boundary.bookmark_names, vec!["feat"]);
        assert_eq!(boundary.excluded_bookmarks, vec!["other-user"]);
    }

    #[test]
    fn leaf_nodes_returns_only_leaves() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_a", &["base"]),
                    make_segment(&["leaf-1"], "ch_b", &["leaf 1"]),
                ],
            },
            BranchStack {
                segments: vec![make_segment(&["leaf-2"], "ch_c", &["leaf 2"])],
            },
        ]);

        let layout = build_layout(&graph);
        let leaves = layout.leaf_nodes();
        assert_eq!(leaves.len(), 2);
        assert!(leaves.iter().all(|n| n.is_leaf));
    }

    #[test]
    fn row_of_node_finds_commit_rows() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["base work"]),
                make_segment(&["leaf"], "ch_b", &["leaf work"]),
            ],
        }]);

        let layout = build_layout(&graph);
        // Leaf on top (row 0), base below (row 1), trunk at the bottom.
        assert_eq!(layout.row_of_node(layout.leaves[0]), Some(0));
        assert_eq!(layout.row_of_node(0), Some(2));
    }

    #[test]
    fn nested_siblings_layout() {
        // trunk → auth → {email chain (newest), api chain}; the api chain
        // itself forks into {integration (newest), ratelimit}.
        let auth = make_segment_at(&["auth"], "ch_auth", &["auth work"], "2026-01-01T00:00:00Z");
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    auth.clone(),
                    make_segment_at(
                        &["email"],
                        "ch_email",
                        &["email work"],
                        "2026-06-01T00:00:00Z",
                    ),
                ],
            },
            BranchStack {
                segments: vec![
                    auth.clone(),
                    make_segment_at(&["api"], "ch_api", &["api work"], "2026-02-01T00:00:00Z"),
                    make_segment_at(
                        &["integration"],
                        "ch_int",
                        &["integration work"],
                        "2026-04-01T00:00:00Z",
                    ),
                ],
            },
            BranchStack {
                segments: vec![
                    auth,
                    make_segment_at(&["api"], "ch_api", &["api work"], "2026-02-01T00:00:00Z"),
                    make_segment_at(
                        &["ratelimit"],
                        "ch_rate",
                        &["ratelimit work"],
                        "2026-03-01T00:00:00Z",
                    ),
                ],
            },
        ]);

        let layout = build_layout(&graph);

        insta::assert_snapshot!(rows_to_string(&layout));

        // Leaves in display order.
        let leaves = layout.leaf_nodes();
        let names: Vec<_> = leaves.iter().map(|l| l.bookmark_names[0].clone()).collect();
        assert_eq!(names, vec!["email", "integration", "ratelimit"]);
    }
}
