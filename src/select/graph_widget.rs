//! Screen 1: Graph view widget.
//!
//! Renders the positioned graph layout as a tree with Unicode box-drawing
//! characters. Users navigate with arrow keys to select a leaf node.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::graph_layout::GraphLayout;
use super::graph_layout::path_to_leaf;

/// State for the graph view widget.
#[derive(Debug)]
pub struct GraphViewState {
    /// Index of the currently selected leaf node (into
    /// `GraphLayout::leaf_nodes()`).
    pub selected_leaf: usize,
    /// Scroll offset (row from bottom that is at the top of the viewport).
    pub scroll_offset: usize,
}

impl GraphViewState {
    pub fn new() -> Self {
        Self {
            selected_leaf: 0,
            scroll_offset: 0,
        }
    }
}

/// Width of one column in characters (node char + spacing).
const COL_WIDTH: usize = 4;
/// Characters used for rendering.
const NODE_CHAR: &str = "\u{25cb}"; // ○
const TRUNK_CHAR: &str = "\u{25c6}"; // ◆
const VERT_LINE: &str = "\u{2502}"; // │
const BRANCH_TEE: &str = "\u{251c}"; // ├
const BRANCH_HORIZ: &str = "\u{2500}"; // ─
const BRANCH_CORNER: &str = "\u{256f}"; // ╯  (UP+LEFT arc — endpoint of branch)
const BRANCH_SRC: &str = "\u{2570}"; // ╰  (UP+RIGHT arc — source with no vertical above)
const CROSS: &str = "\u{253c}"; // ┼  (horizontal branch crossing a vertical edge)

/// Number of display lines for a graph with the given number of rows.
/// Each row produces a node line, plus a connector line between adjacent rows.
pub fn display_line_count(total_rows: usize) -> usize {
    if total_rows == 0 {
        0
    } else {
        // node lines + connector lines between them
        total_rows + total_rows.saturating_sub(1)
    }
}

/// Renders the graph layout with cursor highlighting.
pub struct GraphWidget<'a> {
    layout: &'a GraphLayout,
    state: &'a GraphViewState,
}

impl<'a> GraphWidget<'a> {
    pub fn new(layout: &'a GraphLayout, state: &'a GraphViewState) -> Self {
        Self { layout, state }
    }

    /// Build the lines for the graph, bottom-to-top (trunk at bottom).
    /// Returns lines in display order (top of screen = highest row).
    ///
    /// For each graph row, renders a node line. Between adjacent rows, renders
    /// a connector line showing │ for vertical edges and ├─╮ for branches.
    fn build_lines(&self) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        if self.layout.nodes.is_empty() {
            return lines;
        }

        // Compute the selected path for highlighting.
        let leaves = self.layout.leaf_nodes();
        let selected_leaf = leaves.get(self.state.selected_leaf);
        let selected_path: HashSet<(usize, usize)> = if let Some(leaf) = selected_leaf {
            path_to_leaf(self.layout, leaf.row, leaf.col)
                .iter()
                .map(|n| (n.row, n.col))
                .collect()
        } else {
            HashSet::new()
        };

        // Edges on the selected path (for connector highlighting).
        let selected_edges: HashSet<(usize, usize, usize, usize)> = self
            .layout
            .edges
            .iter()
            .filter(|e| {
                selected_path.contains(&(e.from_row, e.from_col))
                    && selected_path.contains(&(e.to_row, e.to_col))
            })
            .map(|e| (e.from_row, e.from_col, e.to_row, e.to_col))
            .collect();

        let max_row = self.layout.total_rows.saturating_sub(1);

        for row in (0..=max_row).rev() {
            // Node line.
            lines.push(self.build_node_line(row, &selected_path, selected_leaf));

            // Connector line between this row and the one below.
            if row > 0 {
                lines.push(self.build_connector_line(row, row - 1, &selected_edges));
            }
        }

        lines
    }

    /// Render a single node line for the given row.
    fn build_node_line(
        &self,
        row: usize,
        selected_path: &HashSet<(usize, usize)>,
        selected_leaf: Option<&&super::graph_layout::LayoutNode>,
    ) -> Line<'a> {
        let mut spans: Vec<Span> = Vec::new();
        let mut labels: Vec<(String, bool)> = Vec::new(); // (label, is_on_path)

        for col in 0..self.layout.total_cols {
            let is_on_path = selected_path.contains(&(row, col));

            if let Some(node) = self.layout.node_at(row, col) {
                let node_char = if node.is_trunk { TRUNK_CHAR } else { NODE_CHAR };

                let is_selected_leaf =
                    selected_leaf.is_some_and(|l| l.row == node.row && l.col == node.col);

                let style = if is_selected_leaf {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_on_path {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                spans.push(Span::styled(format!("{node_char:<COL_WIDTH$}"), style));

                if is_on_path || is_selected_leaf {
                    let label = self.node_label(node);
                    if !label.is_empty() {
                        labels.push((label, true));
                    }
                }
            } else {
                // Empty column — just space.
                spans.push(Span::raw(format!("{:<COL_WIDTH$}", " ")));
            }
        }

        // Append labels after all columns.
        for (i, (label, on_path)) in labels.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if *on_path {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(label.clone(), style));
        }

        Line::from(spans)
    }

    /// Render the connector line between row_above and row_below.
    ///
    /// Shows │ for vertical edges, ├─╮ for branch forks.
    fn build_connector_line(
        &self,
        row_above: usize,
        row_below: usize,
        selected_edges: &HashSet<(usize, usize, usize, usize)>,
    ) -> Line<'a> {
        // Determine what character each column gets.
        let total_cols = self.layout.total_cols;
        let mut col_chars: Vec<&str> = vec![" "; total_cols];
        let mut col_on_path: Vec<bool> = vec![false; total_cols];

        for edge in &self.layout.edges {
            let is_selected =
                selected_edges.contains(&(edge.from_row, edge.from_col, edge.to_row, edge.to_col));

            if edge.from_col == edge.to_col {
                // Vertical edge — check if it spans this connector gap.
                let col = edge.from_col;
                if edge.from_row <= row_below && edge.to_row >= row_above {
                    col_chars[col] = VERT_LINE;
                    if is_selected {
                        col_on_path[col] = true;
                    }
                }
            } else {
                // Cross-column (branch) edge at this connector level.
                if edge.from_row == row_below && edge.to_row == row_above {
                    let min_col = edge.from_col.min(edge.to_col);
                    let max_col = edge.from_col.max(edge.to_col);

                    // At from_col: ├ if there's already a vertical edge (child
                    // above in the same column), ╰ if only the branch exits here.
                    if col_chars[edge.from_col] == VERT_LINE {
                        col_chars[edge.from_col] = BRANCH_TEE; // ├
                    } else {
                        col_chars[edge.from_col] = BRANCH_SRC; // ╰
                    }
                    if is_selected {
                        col_on_path[edge.from_col] = true;
                    }

                    // At to_col: ╯
                    col_chars[edge.to_col] = BRANCH_CORNER;
                    if is_selected {
                        col_on_path[edge.to_col] = true;
                    }

                    // Between: ─, or ┼ where a vertical edge crosses
                    for c in (min_col + 1)..max_col {
                        col_chars[c] = if col_chars[c] == VERT_LINE {
                            CROSS
                        } else {
                            BRANCH_HORIZ
                        };
                        if is_selected {
                            col_on_path[c] = true;
                        }
                    }
                }
            }
        }

        let mut spans: Vec<Span> = Vec::new();
        for col in 0..total_cols {
            let style = if col_on_path[col] {
                Style::default().fg(Color::Cyan)
            } else if col_chars[col] != " " {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            // For horizontal branch characters, fill the full column width with
            // ─ to create a continuous line. Source chars (├, ╰) and crossings
            // (┼) also extend rightward with dashes.
            let cell = if col_chars[col] == BRANCH_HORIZ {
                BRANCH_HORIZ.repeat(COL_WIDTH)
            } else if col_chars[col] == BRANCH_TEE
                || col_chars[col] == BRANCH_SRC
                || col_chars[col] == CROSS
            {
                format!("{}{}", col_chars[col], BRANCH_HORIZ.repeat(COL_WIDTH - 1))
            } else {
                format!("{:<COL_WIDTH$}", col_chars[col])
            };

            spans.push(Span::styled(cell, style));
        }

        Line::from(spans)
    }

    fn node_label(&self, node: &super::graph_layout::LayoutNode) -> String {
        let mut parts = Vec::new();

        if !node.bookmark_names.is_empty() {
            parts.push(node.bookmark_names.join(", "));
        }

        if node.is_trunk {
            if parts.is_empty() {
                parts.push("trunk".to_string());
            }
        } else {
            let summary = &node.summary;
            if summary != "(no description)" {
                parts.push(format!("\"{summary}\""));
            }
        }

        parts.join("  ")
    }
}

impl Widget for GraphWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.build_lines();

        // Apply scroll offset.
        let visible_height = area.height as usize;
        let total = lines.len();
        let start = if total > visible_height {
            self.state.scroll_offset.min(total - visible_height)
        } else {
            0
        };

        for (i, line) in lines.iter().skip(start).take(visible_height).enumerate() {
            let y = area.y + i as u16;
            if y >= area.y + area.height {
                break;
            }
            buf.set_line(area.x, y, line, area.width);
        }
    }
}

/// Build a help line for the bottom of the graph view.
pub fn graph_help_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " \u{2190}\u{2192}/hl",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" navigate  "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" select  "),
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit"),
    ])
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use super::*;
    use crate::graph::types::BookmarkSegment;
    use crate::graph::types::BranchStack;
    use crate::graph::types::ChangeGraph;
    use crate::graph::types::SegmentCommit;
    use crate::select::graph_layout::build_layout;

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
            bookmark_names: names.iter().map(|s| s.to_string()).collect(),
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

    fn render_to_string(graph: &ChangeGraph) -> String {
        let layout = build_layout(graph);
        let state = GraphViewState::new();
        let widget = GraphWidget::new(&layout, &state);
        let lines = widget.build_lines();
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn linear_stack_has_connectors() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["add base"]),
                make_segment(&["leaf"], "ch_b", &["add leaf"]),
            ],
        }]);

        let layout = build_layout(&graph);
        let state = GraphViewState::new();
        let widget = GraphWidget::new(&layout, &state);
        let lines = widget.build_lines();

        // 3 rows → 3 node lines + 2 connector lines = 5 display lines.
        assert_eq!(lines.len(), 5);

        let text = render_to_string(&graph);
        // Should contain vertical connector lines.
        assert!(text.contains('\u{2502}'), "expected │ in output: {text}");
    }

    #[test]
    fn branching_shows_fork_characters() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment(&["alpha"], "ch_alpha", &["alpha work"])],
            },
            BranchStack {
                segments: vec![make_segment(&["beta"], "ch_beta", &["beta work"])],
            },
        ]);

        let text = render_to_string(&graph);
        // Should contain branch fork characters.
        assert!(text.contains('\u{251c}'), "expected ├ in output:\n{text}");
        assert!(text.contains('\u{256f}'), "expected ╯ in output:\n{text}");
    }

    #[test]
    fn renders_to_buffer() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(&["feat"], "ch_a", &["my feature"])],
        }]);

        let layout = build_layout(&graph);
        let state = GraphViewState::new();
        let widget = GraphWidget::new(&layout, &state);

        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            content.contains('\u{25cb}') || content.contains('\u{25c6}'),
            "expected node characters in rendered output"
        );
    }

    #[test]
    fn shared_root_shows_branch() {
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

        let text = render_to_string(&graph);
        // Should have both ○ nodes, ◆ trunk, │ connectors, and ├╮ branch.
        assert!(text.contains('\u{25cb}'), "expected ○:\n{text}");
        assert!(text.contains('\u{25c6}'), "expected ◆:\n{text}");
        assert!(text.contains('\u{2502}'), "expected │:\n{text}");
        assert!(text.contains('\u{251c}'), "expected ├:\n{text}");
        assert!(text.contains('\u{256f}'), "expected ╯:\n{text}");
    }

    #[test]
    fn display_line_count_correct() {
        assert_eq!(display_line_count(0), 0);
        assert_eq!(display_line_count(1), 1);
        assert_eq!(display_line_count(2), 3);
        assert_eq!(display_line_count(3), 5);
    }
}
