//! Convert a `ChangeGraph` into a 2D layout for rendering.
//!
//! Walks from trunk upward, assigning row/column positions to each commit node.
//! The first stack gets column 0; branches fork rightward at split points.

use std::collections::HashMap;

use crate::graph::types::ChangeGraph;

/// A positioned node in the 2D graph layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutNode {
    /// Row in the layout (0 = bottommost / trunk).
    pub row: usize,
    /// Column in the layout (0 = leftmost / main path).
    pub col: usize,
    /// The jj change ID for this commit.
    pub change_id: String,
    /// The jj commit ID.
    pub commit_id: String,
    /// First line of the commit description.
    pub summary: String,
    /// Bookmark names on this commit (may be empty).
    pub bookmark_names: Vec<String>,
    /// Whether this node is the trunk node.
    pub is_trunk: bool,
    /// Whether this node is a leaf (no children).
    pub is_leaf: bool,
    /// Index of the stack this node belongs to.
    pub stack_index: usize,
}

/// An edge connecting two nodes in the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEdge {
    /// Row of the parent (lower) node.
    pub from_row: usize,
    /// Column of the parent (lower) node.
    pub from_col: usize,
    /// Row of the child (upper) node.
    pub to_row: usize,
    /// Column of the child (upper) node.
    pub to_col: usize,
}

/// The complete 2D layout of the change graph.
#[derive(Debug, Clone)]
pub struct GraphLayout {
    /// All nodes, ordered by row (bottom to top).
    pub nodes: Vec<LayoutNode>,
    /// All edges connecting nodes.
    pub edges: Vec<LayoutEdge>,
    /// Total number of rows.
    pub total_rows: usize,
    /// Total number of columns.
    pub total_cols: usize,
}

impl GraphLayout {
    /// Find a node by its (row, col) position.
    pub fn node_at(&self, row: usize, col: usize) -> Option<&LayoutNode> {
        self.nodes.iter().find(|n| n.row == row && n.col == col)
    }

    /// Get the index of a node in the nodes vec by row/col.
    #[expect(dead_code, reason = "available for future navigation features")]
    pub fn node_index_at(&self, row: usize, col: usize) -> Option<usize> {
        self.nodes.iter().position(|n| n.row == row && n.col == col)
    }

    /// Return all leaf nodes (selectable branch tips), sorted left-to-right by
    /// column so that index 0 is the leftmost leaf.
    pub fn leaf_nodes(&self) -> Vec<&LayoutNode> {
        let mut leaves: Vec<&LayoutNode> = self.nodes.iter().filter(|n| n.is_leaf).collect();
        leaves.sort_by_key(|n| n.col);
        leaves
    }
}

/// Build a 2D graph layout from a `ChangeGraph`.
///
/// The layout places trunk at the bottom (row 0) and grows upward. Each stack
/// gets its own column, with shared segments placed in the leftmost column that
/// uses them.
pub fn build_layout(graph: &ChangeGraph) -> GraphLayout {
    if graph.stacks.is_empty() {
        return GraphLayout {
            nodes: vec![],
            edges: vec![],
            total_rows: 0,
            total_cols: 0,
        };
    }

    // Track which commit_ids we've already placed (for shared segments).
    // We key by commit_id since it's unique per commit, while change_id is
    // shared by all commits in a segment.
    let mut placed: HashMap<String, (usize, usize)> = HashMap::new();
    let mut nodes: Vec<LayoutNode> = Vec::new();
    let mut edges: Vec<LayoutEdge> = Vec::new();

    // We build a trunk node at row 0.
    let trunk_row = 0;
    nodes.push(LayoutNode {
        row: trunk_row,
        col: 0,
        change_id: String::new(),
        commit_id: String::new(),
        summary: "trunk".to_string(),
        bookmark_names: vec![],
        is_trunk: true,
        is_leaf: false,
        stack_index: 0,
    });

    let num_stacks = graph.stacks.len();
    let mut max_row: usize = 0;

    for (stack_idx, stack) in graph.stacks.iter().enumerate() {
        let col = stack_idx;
        // Depth from trunk — each commit gets the next depth. For shared
        // commits (already placed), we resume from the shared commit's row.
        let mut depth: usize = 1;
        let mut prev_row = trunk_row;
        let mut prev_col: usize = 0;

        for (seg_idx, segment) in stack.segments.iter().enumerate() {
            let is_last_segment = seg_idx == stack.segments.len() - 1;

            // Process commits in this segment (they are newest-first in the
            // segment, but we want to lay them out bottom-to-top, so reverse).
            let commits: Vec<_> = segment.commits.iter().rev().collect();

            for (commit_idx, commit) in commits.iter().enumerate() {
                let is_last_commit_in_segment = commit_idx == commits.len() - 1;

                // Check if this commit was already placed (shared segment).
                if let Some(&(existing_row, existing_col)) = placed.get(&commit.commit_id) {
                    // Shared node: continue from its depth.
                    prev_row = existing_row;
                    prev_col = existing_col;
                    depth = existing_row + 1;
                    continue;
                }

                let bookmark_names = if is_last_commit_in_segment {
                    segment.bookmark_names.clone()
                } else {
                    vec![]
                };

                let summary = commit
                    .description
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .unwrap_or("(no description)")
                    .to_string();

                let is_leaf = is_last_segment && is_last_commit_in_segment;

                let row = depth;
                depth += 1;

                nodes.push(LayoutNode {
                    row,
                    col,
                    change_id: commit.change_id.clone(),
                    commit_id: commit.commit_id.clone(),
                    summary,
                    bookmark_names,
                    is_trunk: false,
                    is_leaf,
                    stack_index: stack_idx,
                });

                edges.push(LayoutEdge {
                    from_row: prev_row,
                    from_col: prev_col,
                    to_row: row,
                    to_col: col,
                });

                placed.insert(commit.commit_id.clone(), (row, col));
                max_row = max_row.max(row);
                prev_row = row;
                prev_col = col;
            }
        }
    }

    // Sort nodes by row for consistent iteration.
    nodes.sort_by_key(|n| (n.row, n.col));

    let total_rows = max_row + 1;
    let total_cols = num_stacks.max(1);

    GraphLayout {
        nodes,
        edges,
        total_rows,
        total_cols,
    }
}

/// Collect all nodes on the path from trunk to a given leaf node.
///
/// Returns nodes in trunk-to-leaf order (bottom-to-top in the layout).
pub fn path_to_leaf(layout: &GraphLayout, leaf_row: usize, leaf_col: usize) -> Vec<&LayoutNode> {
    let mut path = Vec::new();

    // Walk backward from leaf to trunk using edges.
    let mut current_row = leaf_row;
    let mut current_col = leaf_col;

    while let Some(node) = layout.node_at(current_row, current_col) {
        path.push(node);
        if node.is_trunk {
            break;
        }

        // Find the edge that leads to this node.
        let Some(edge) = layout
            .edges
            .iter()
            .find(|e| e.to_row == current_row && e.to_col == current_col)
        else {
            break;
        };

        current_row = edge.from_row;
        current_col = edge.from_col;
    }

    path.reverse(); // trunk-to-leaf order
    path
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
                    author_name: "Test".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn empty_graph_layout() {
        let graph = make_graph(vec![]);
        let layout = build_layout(&graph);
        assert!(layout.nodes.is_empty());
        assert_eq!(layout.total_rows, 0);
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
        assert_eq!(layout.total_rows, 3);
        assert_eq!(layout.total_cols, 1);

        // Row 0 is trunk.
        assert!(layout.nodes[0].is_trunk);

        // Row 1 is base commit.
        assert_eq!(layout.nodes[1].row, 1);
        assert_eq!(layout.nodes[1].bookmark_names, vec!["base"]);
        assert!(!layout.nodes[1].is_leaf);

        // Row 2 is leaf commit.
        assert_eq!(layout.nodes[2].row, 2);
        assert_eq!(layout.nodes[2].bookmark_names, vec!["leaf"]);
        assert!(layout.nodes[2].is_leaf);

        // 2 edges: trunk→base, base→leaf.
        assert_eq!(layout.edges.len(), 2);
    }

    #[test]
    fn two_branching_stacks() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment(&["alpha"], "ch_alpha", &["alpha work"])],
            },
            BranchStack {
                segments: vec![make_segment(&["beta"], "ch_beta", &["beta work"])],
            },
        ]);

        let layout = build_layout(&graph);

        // trunk + 2 commits = 3 nodes
        assert_eq!(layout.nodes.len(), 3);
        assert_eq!(layout.total_cols, 2);

        // alpha is in col 0
        let alpha = layout
            .nodes
            .iter()
            .find(|n| n.bookmark_names.contains(&"alpha".to_string()))
            .unwrap();
        assert_eq!(alpha.col, 0);
        assert!(alpha.is_leaf);

        // beta is in col 1
        let beta = layout
            .nodes
            .iter()
            .find(|n| n.bookmark_names.contains(&"beta".to_string()))
            .unwrap();
        assert_eq!(beta.col, 1);
        assert!(beta.is_leaf);
    }

    #[test]
    fn shared_root_segment() {
        // Two stacks sharing a root segment (same change_id).
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared base"]),
                    make_segment(&["feat-a"], "ch_a", &["feature a"]),
                ],
            },
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared base"]),
                    make_segment(&["feat-b"], "ch_b", &["feature b"]),
                ],
            },
        ]);

        let layout = build_layout(&graph);

        // trunk + shared_base + feat_a + feat_b = 4 nodes
        assert_eq!(layout.nodes.len(), 4);

        // The shared base should only appear once.
        let shared_nodes: Vec<_> = layout
            .nodes
            .iter()
            .filter(|n| n.change_id == "ch_shared")
            .collect();
        assert_eq!(shared_nodes.len(), 1);
        assert_eq!(shared_nodes[0].col, 0); // placed in first stack's column

        // Both feat-a and feat-b should be separate.
        let feat_a = layout.nodes.iter().find(|n| n.change_id == "ch_a").unwrap();
        let feat_b = layout.nodes.iter().find(|n| n.change_id == "ch_b").unwrap();
        assert_eq!(feat_a.col, 0);
        assert_eq!(feat_b.col, 1);
        assert!(feat_a.is_leaf);
        assert!(feat_b.is_leaf);
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

        // trunk + 2 commits = 3 nodes
        assert_eq!(layout.nodes.len(), 3);

        // Commits are laid out bottom-to-top (reversed from newest-first).
        // "first commit" should be at a lower row than "second commit".
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
        assert!(first.row < second.row);

        // Only the last commit in the segment gets bookmark names.
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
        let leaf = layout.leaf_nodes()[0];
        let path = path_to_leaf(&layout, leaf.row, leaf.col);

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
        let feat_b = layout.nodes.iter().find(|n| n.change_id == "ch_b").unwrap();
        let path = path_to_leaf(&layout, feat_b.row, feat_b.col);

        // trunk → shared → feat_b
        assert_eq!(path.len(), 3);
        assert!(path[0].is_trunk);
        assert_eq!(path[1].change_id, "ch_shared");
        assert_eq!(path[2].change_id, "ch_b");
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
}
