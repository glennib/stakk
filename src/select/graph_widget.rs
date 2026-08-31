//! Screen 1: jj-log-style graph view widget.
//!
//! Renders the layout's display rows top-down: one row per commit with a
//! `│ `-cell gutter on the left, `├─╯` connector rows where sibling subtrees
//! merge, and `⋯ n commits` rows for collapsed runs of unselected commits.
//! Every row permanently carries its change id, bookmark names and
//! description; leaf navigation only re-colors the selected trunk→leaf path.

use std::collections::HashMap;
use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use crate::graph::layout::CONNECTOR_TAIL;
use crate::graph::layout::CONNECTOR_TEE;
use crate::graph::layout::ELLIPSIS;
use crate::graph::layout::GUTTER_CELL;
use crate::graph::layout::GraphLayout;
use crate::graph::layout::GraphRow;
use crate::graph::layout::LEAF_MARKER;
use crate::graph::layout::LayoutNode;
use crate::graph::layout::NODE_OTHER;
use crate::graph::layout::NODE_SELECTED;
use crate::graph::layout::TRUNK_CHAR;

/// Total characters of change id shown per row (matching jj log's default):
/// the shortest unique prefix, padded with dimmed characters from the full
/// id up to this width.
const CHANGE_ID_WIDTH: usize = 8;

/// State for the graph view widget.
#[derive(Debug)]
pub struct GraphViewState {
    /// Index of the currently selected leaf (into `GraphLayout::leaves`).
    pub selected_leaf: usize,
}

impl GraphViewState {
    pub fn new() -> Self {
        Self { selected_leaf: 0 }
    }
}

/// A row after collapsing, ready for rendering.
enum DisplayRow<'a> {
    /// A commit (or trunk) row.
    Commit {
        node_idx: usize,
        node: &'a LayoutNode,
        col: usize,
    },
    /// A collapsed run of `count` unselected commits.
    Collapsed { count: usize, col: usize },
    /// A connector row (`├─╯`) merging column `col` into `col - 1`.
    Connector { col: usize },
}

/// Collapse runs of unselected commits into `⋯ n commits` rows.
///
/// A commit row collapses when it is not on the selected path, is neither a
/// segment boundary (no bookmark names) nor a leaf nor trunk. Consecutive
/// collapsible rows in the same column merge into one `Collapsed` row.
fn collapse_rows<'a>(layout: &'a GraphLayout, path: &HashSet<usize>) -> Vec<DisplayRow<'a>> {
    let mut out: Vec<DisplayRow<'a>> = Vec::new();
    let mut run: Option<(usize, usize)> = None; // (col, count)

    let flush = |run: &mut Option<(usize, usize)>, out: &mut Vec<DisplayRow<'a>>| {
        if let Some((col, count)) = run.take() {
            out.push(DisplayRow::Collapsed { count, col });
        }
    };

    for row in &layout.rows {
        match *row {
            GraphRow::Commit { node, col } => {
                let n = &layout.nodes[node];
                let collapsible = !path.contains(&node)
                    && !n.is_trunk
                    && !n.is_leaf
                    && n.bookmark_names.is_empty();
                if collapsible {
                    match &mut run {
                        Some((c, count)) if *c == col => *count += 1,
                        _ => {
                            flush(&mut run, &mut out);
                            run = Some((col, 1));
                        }
                    }
                } else {
                    flush(&mut run, &mut out);
                    out.push(DisplayRow::Commit {
                        node_idx: node,
                        node: n,
                        col,
                    });
                }
            }
            GraphRow::Connector { col } => {
                flush(&mut run, &mut out);
                out.push(DisplayRow::Connector { col });
            }
        }
    }
    flush(&mut run, &mut out);

    out
}

/// Number of display rows when the given leaf is selected (its path is fully
/// expanded; everything else collapses).
pub fn collapsed_height(layout: &GraphLayout, leaf: usize) -> usize {
    let path: HashSet<usize> = layout.path_node_indices(leaf).into_iter().collect();
    collapse_rows(layout, &path).len()
}

/// Maximum display height over all selectable leaves — sizing the viewport
/// to this means switching leaves never needs a viewport resize.
pub fn max_collapsed_height(layout: &GraphLayout) -> usize {
    layout
        .leaves
        .iter()
        .map(|&leaf| collapsed_height(layout, leaf))
        .max()
        .unwrap_or(0)
}

/// Renders the graph layout with selected-path highlighting.
pub struct GraphWidget<'a> {
    layout: &'a GraphLayout,
    state: &'a GraphViewState,
}

impl<'a> GraphWidget<'a> {
    pub fn new(layout: &'a GraphLayout, state: &'a GraphViewState) -> Self {
        Self { layout, state }
    }

    /// Build the display lines (top to bottom) and the display-row index of
    /// the selected leaf (for scrolling).
    fn build_lines(&self) -> (Vec<Line<'a>>, usize) {
        let Some(&leaf) = self.layout.leaves.get(self.state.selected_leaf) else {
            return (vec![], 0);
        };

        let path_indices = self.layout.path_node_indices(leaf);
        let path: HashSet<usize> = path_indices.iter().copied().collect();
        let rows = collapse_rows(self.layout, &path);

        // Display row index per node (commit rows only).
        let row_of: HashMap<usize, usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                DisplayRow::Commit { node_idx, .. } => Some((*node_idx, i)),
                _ => None,
            })
            .collect();
        let col_of: HashMap<usize, usize> = rows
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Commit { node_idx, col, .. } => Some((*node_idx, *col)),
                _ => None,
            })
            .collect();

        // Gutter cells and connector rows that carry the selected path.
        // For each parent→child pair on the path (parent below, child
        // above): a same-column child's edge runs vertically through the
        // child's column in every row between them; a sibling child's edge
        // passes through its connector row (directly under the child) and
        // then continues in the parent's column.
        let mut path_gutter: HashSet<(usize, usize)> = HashSet::new();
        let mut path_connectors: HashSet<usize> = HashSet::new();
        for pair in path_indices.windows(2) {
            let (parent, child) = (pair[0], pair[1]);
            let (Some(&pr), Some(&cr)) = (row_of.get(&parent), row_of.get(&child)) else {
                continue;
            };
            let (Some(&pcol), Some(&ccol)) = (col_of.get(&parent), col_of.get(&child)) else {
                continue;
            };
            if pcol == ccol {
                for r in (cr + 1)..pr {
                    path_gutter.insert((r, ccol));
                }
            } else {
                if matches!(rows.get(cr + 1), Some(DisplayRow::Connector { col }) if *col == ccol) {
                    path_connectors.insert(cr + 1);
                }
                for r in (cr + 2)..pr {
                    path_gutter.insert((r, pcol));
                }
            }
        }

        let leaf_row = row_of.get(&leaf).copied().unwrap_or(0);

        let lines = rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                Self::build_row_line(r, row, &path, leaf, &path_gutter, &path_connectors)
            })
            .collect();

        (lines, leaf_row)
    }

    fn build_row_line(
        r: usize,
        row: &DisplayRow<'a>,
        path: &HashSet<usize>,
        leaf: usize,
        path_gutter: &HashSet<(usize, usize)>,
        path_connectors: &HashSet<usize>,
    ) -> Line<'a> {
        let on_path_style = Style::default().fg(Color::Cyan);
        let dim_style = Style::default().fg(Color::DarkGray);

        let mut spans: Vec<Span> = vec![Span::raw(" ")];

        let gutter = |spans: &mut Vec<Span>, cols: usize| {
            for g in 0..cols {
                let style = if path_gutter.contains(&(r, g)) {
                    on_path_style
                } else {
                    dim_style
                };
                spans.push(Span::styled(GUTTER_CELL, style));
            }
        };

        match row {
            DisplayRow::Commit {
                node_idx,
                node,
                col,
            } => {
                gutter(&mut spans, *col);

                let is_on_path = path.contains(node_idx);
                let is_selected_leaf = *node_idx == leaf;

                let glyph = if node.is_trunk {
                    TRUNK_CHAR
                } else if is_on_path {
                    NODE_SELECTED
                } else {
                    NODE_OTHER
                };
                let glyph_style = if is_selected_leaf {
                    on_path_style.add_modifier(Modifier::BOLD)
                } else if is_on_path {
                    on_path_style
                } else {
                    dim_style
                };
                spans.push(Span::styled(glyph, glyph_style));
                spans.push(Span::raw("  "));

                spans.extend(Self::label_spans(node, is_on_path));

                if is_selected_leaf {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        LEAF_MARKER,
                        on_path_style.add_modifier(Modifier::BOLD),
                    ));
                }
            }
            DisplayRow::Collapsed { count, col } => {
                gutter(&mut spans, *col);
                spans.push(Span::styled(ELLIPSIS, dim_style));
                let noun = if *count == 1 { "commit" } else { "commits" };
                spans.push(Span::styled(
                    format!("  {count} {noun}"),
                    dim_style.add_modifier(Modifier::DIM),
                ));
            }
            DisplayRow::Connector { col } => {
                gutter(&mut spans, col.saturating_sub(1));
                // The ├ cell carries the parent column's vertical line, so
                // it stays path-colored when the path passes through that
                // column even if this connector itself is off-path.
                let tee_on_path = path_connectors.contains(&r)
                    || path_gutter.contains(&(r, col.saturating_sub(1)));
                spans.push(Span::styled(
                    CONNECTOR_TEE,
                    if tee_on_path {
                        on_path_style
                    } else {
                        dim_style
                    },
                ));
                spans.push(Span::styled(
                    CONNECTOR_TAIL,
                    if path_connectors.contains(&r) {
                        on_path_style
                    } else {
                        dim_style
                    },
                ));
            }
        }

        Line::from(spans)
    }

    fn label_spans(node: &LayoutNode, is_on_path: bool) -> Vec<Span<'static>> {
        let mut spans = Vec::new();

        let name_style = if is_on_path {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let text_style = if is_on_path {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        if node.is_trunk {
            spans.push(Span::styled("trunk", text_style));
            return spans;
        }

        spans.extend(Self::change_id_spans(node, is_on_path));
        spans.push(Span::raw("  "));

        if !node.bookmark_names.is_empty() {
            spans.push(Span::styled(node.bookmark_names.join(", "), name_style));
            spans.push(Span::raw("  "));
        }

        if node.summary == "(no description)" {
            spans.push(Span::styled(
                "(no description set)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ));
        } else {
            spans.push(Span::styled(format!("\"{}\"", node.summary), text_style));
        }

        spans
    }

    /// The change id rendered jj-log style: the shortest unique prefix
    /// bright, the rest of the id dimmed, [`CHANGE_ID_WIDTH`] characters in
    /// total (more when the unique prefix itself is longer).
    fn change_id_spans(node: &LayoutNode, is_on_path: bool) -> Vec<Span<'static>> {
        let (prefix_style, rest_style) = if is_on_path {
            (
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (
                Style::default().fg(Color::DarkGray),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )
        };

        let prefix = node.short_change_id.as_str();
        let rest: String = node
            .change_id
            .strip_prefix(prefix)
            .unwrap_or("")
            .chars()
            .take(CHANGE_ID_WIDTH.saturating_sub(prefix.chars().count()))
            .collect();

        let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
        if !rest.is_empty() {
            spans.push(Span::styled(rest, rest_style));
        }
        spans
    }
}

impl Widget for GraphWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (lines, leaf_row) = self.build_lines();

        // Scroll so the selected leaf sits at the top of the viewport when
        // the content overflows — its path extends downward from there.
        let visible_height = area.height as usize;
        let total = lines.len();
        let start = if total > visible_height {
            leaf_row.min(total - visible_height)
        } else {
            0
        };

        for (i, line) in lines.iter().skip(start).take(visible_height).enumerate() {
            let y = area.y + u16::try_from(i).expect("line index fits in u16");
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
            " \u{2190}\u{2192}\u{2191}\u{2193}/hjkl",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" leaf  "),
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
    use crate::graph::layout::build_layout;
    use crate::graph::types::BookmarkSegment;
    use crate::graph::types::BranchStack;
    use crate::graph::types::ChangeGraph;
    use crate::graph::types::SegmentCommit;

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: HashSet::new(),
            bookmark_remote_states: HashMap::new(),
            excluded_bookmarks: Vec::new(),
            excluded_head_count: 0,
            stacks,
        }
    }

    fn make_segment(names: &[&str], change_id: &str, descriptions: &[&str]) -> BookmarkSegment {
        make_segment_at(names, change_id, descriptions, "T")
    }

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
                    remote_bookmark_names: vec![],
                })
                .collect(),
        }
    }

    fn render_to_string_with_state(graph: &ChangeGraph, state: &GraphViewState) -> String {
        let layout = build_layout(graph);
        let widget = GraphWidget::new(&layout, state);
        let (lines, _) = widget.build_lines();
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_to_string(graph: &ChangeGraph) -> String {
        render_to_string_with_state(graph, &GraphViewState::new())
    }

    #[test]
    fn linear_stack_single_column() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["add base"]),
                make_segment(&["leaf"], "ch_b", &["add leaf"]),
            ],
        }]);

        insta::assert_snapshot!(render_to_string(&graph));
    }

    #[test]
    fn branching_shows_fork_characters() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        insta::assert_snapshot!(render_to_string(&graph));
    }

    #[test]
    fn collapses_unselected_runs() {
        // The selected (newest) chain stays fully expanded; the unselected
        // chain collapses its unbookmarked middle commits but keeps its leaf
        // and boundary visible.
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["feat"],
                    "ch_feat",
                    &["feat top", "feat mid", "feat bottom"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![
                    make_segment_at(
                        &["other-base"],
                        "ch_ob",
                        &["other base"],
                        "2026-01-01T00:00:00Z",
                    ),
                    make_segment_at(
                        &["other"],
                        "ch_o",
                        &["other top", "other mid 2", "other mid 1"],
                        "2026-01-01T00:00:00Z",
                    ),
                ],
            },
        ]);

        insta::assert_snapshot!(render_to_string(&graph));
    }

    #[test]
    fn selecting_other_leaf_moves_highlight_not_rows() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work", "beta base"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        // Selecting beta expands its run and moves ● + ◀ there; alpha's row
        // text stays in place.
        insta::assert_snapshot!(render_to_string_with_state(
            &graph,
            &GraphViewState { selected_leaf: 1 }
        ));
    }

    #[test]
    fn nested_fork_gutters() {
        let auth = make_segment_at(&["auth"], "ch_auth", &["auth work"], "2026-01-01T00:00:00Z");
        let api = make_segment_at(&["api"], "ch_api", &["api work"], "2026-02-01T00:00:00Z");
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
                    api.clone(),
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
                    api,
                    make_segment_at(
                        &["ratelimit"],
                        "ch_rate",
                        &["ratelimit work"],
                        "2026-03-01T00:00:00Z",
                    ),
                ],
            },
        ]);

        insta::assert_snapshot!(render_to_string(&graph));
    }

    #[test]
    fn renders_to_buffer_with_scroll() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        let layout = build_layout(&graph);

        // Viewport shorter than the content: selecting beta (display row 1)
        // anchors it at the top.
        let state = GraphViewState { selected_leaf: 1 };
        let widget = GraphWidget::new(&layout, &state);
        let area = Rect::new(0, 0, 40, 2);
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

        assert!(content.contains("beta"), "expected beta visible: {content}");
        assert!(
            !content.contains("alpha"),
            "expected alpha scrolled out: {content}"
        );
    }

    #[test]
    fn path_styling_is_cyan_and_others_dim() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        let layout = build_layout(&graph);
        let state = GraphViewState::new();
        let widget = GraphWidget::new(&layout, &state);
        let (lines, leaf_row) = widget.build_lines();

        assert_eq!(leaf_row, 0);

        // Selected leaf row: glyph span is cyan + bold.
        let leaf_glyph = &lines[0].spans[1];
        assert_eq!(leaf_glyph.content.as_ref(), NODE_SELECTED);
        assert_eq!(leaf_glyph.style.fg, Some(Color::Cyan));
        assert!(leaf_glyph.style.add_modifier.contains(Modifier::BOLD));

        // Off-path commit row: glyph is ○ and dark gray.
        let other_glyph = &lines[1].spans[2];
        assert_eq!(other_glyph.content.as_ref(), NODE_OTHER);
        assert_eq!(other_glyph.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn change_id_is_two_tone_on_path_and_dim_off_path() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["alpha"],
                    "ch_alpha",
                    &["alpha work"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["beta"],
                    "ch_beta",
                    &["beta work"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        let layout = build_layout(&graph);
        let state = GraphViewState::new();
        let widget = GraphWidget::new(&layout, &state);
        let (lines, _) = widget.build_lines();

        // On-path (selected) row: glyph, spacer, then the shortest unique
        // prefix bright magenta + bold, then the rest of the id (up to 8
        // characters total) dark gray.
        let prefix = &lines[0].spans[3];
        assert_eq!(prefix.content.as_ref(), "ch_a");
        assert_eq!(prefix.style.fg, Some(Color::Magenta));
        assert!(prefix.style.add_modifier.contains(Modifier::BOLD));
        let rest = &lines[0].spans[4];
        assert_eq!(rest.content.as_ref(), "lpha");
        assert_eq!(rest.style.fg, Some(Color::DarkGray));

        // Off-path row (one extra gutter span first): both id spans dark
        // gray, the remainder additionally DIM.
        let prefix = &lines[1].spans[4];
        assert_eq!(prefix.content.as_ref(), "ch_b");
        assert_eq!(prefix.style.fg, Some(Color::DarkGray));
        assert!(!prefix.style.add_modifier.contains(Modifier::BOLD));
        let rest = &lines[1].spans[5];
        assert_eq!(rest.content.as_ref(), "eta");
        assert_eq!(rest.style.fg, Some(Color::DarkGray));
        assert!(rest.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn collapsed_height_and_max() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![make_segment_at(
                    &["long"],
                    "ch_long",
                    &["l4", "l3", "l2", "l1"],
                    "2026-02-01T00:00:00Z",
                )],
            },
            BranchStack {
                segments: vec![make_segment_at(
                    &["short"],
                    "ch_short",
                    &["s1"],
                    "2026-01-01T00:00:00Z",
                )],
            },
        ]);

        let layout = build_layout(&graph);

        // Selecting `long` expands its 4 commits: 4 + short leaf + connector
        // + trunk = 7 rows.
        assert_eq!(collapsed_height(&layout, layout.leaves[0]), 7);
        // Selecting `short` collapses long's 3 unbookmarked commits into one
        // ⋯ row: leaf + ⋯ + short + connector + trunk = 5 rows.
        assert_eq!(collapsed_height(&layout, layout.leaves[1]), 5);
        assert_eq!(max_collapsed_height(&layout), 7);
    }
}
