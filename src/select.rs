//! Interactive bookmark selection with visual graph context.
//!
//! Renders a graph of bookmark stacks using Unicode box-drawing characters
//! and lets the user pick a bookmark to submit. Uses the `console` crate
//! for terminal I/O (cursor control, key reading, styling).

use std::io;

use console::Key;
use console::Term;
use console::style;

use crate::error::StakkError;
use crate::graph::types::ChangeGraph;

/// One selectable entry in the interactive bookmark picker.
#[derive(Debug, Clone)]
pub struct SelectableBookmark {
    /// The bookmark name (first from `segment.bookmark_names`).
    pub bookmark_name: String,
    /// First line of each commit description in the segment, newest-first.
    pub commit_descriptions: Vec<String>,
    /// Which stack this belongs to (index into `ChangeGraph::stacks`).
    pub stack_index: usize,
    /// Position within the stack (0 = closest to trunk).
    pub segment_index: usize,
    /// Total number of segments in this stack.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in tests to verify stack metadata")
    )]
    pub stack_len: usize,
}

/// Extract selectable bookmarks from a change graph.
///
/// Iterates all stacks and segments, producing one entry per segment using
/// the first bookmark name. Segments are emitted in stack order (trunk-to-leaf
/// within each stack).
pub fn collect_selectable_bookmarks(graph: &ChangeGraph) -> Vec<SelectableBookmark> {
    let mut result = Vec::new();

    for (stack_index, stack) in graph.stacks.iter().enumerate() {
        let stack_len = stack.segments.len();
        for (segment_index, segment) in stack.segments.iter().enumerate() {
            let bookmark_name = segment
                .bookmark_names
                .first()
                .cloned()
                .unwrap_or_else(|| "(unnamed)".to_string());

            let commit_descriptions: Vec<String> = segment
                .commits
                .iter()
                .map(|c| {
                    let first_line = c
                        .description
                        .lines()
                        .next()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty());
                    first_line
                        .unwrap_or("(no description)")
                        .to_string()
                })
                .collect();

            result.push(SelectableBookmark {
                bookmark_name,
                commit_descriptions,
                stack_index,
                segment_index,
                stack_len,
            });
        }
    }

    result
}

/// Render the graph to the terminal and return the number of lines written.
///
/// The focused bookmark is shown in green/bold. All ancestors of the focused
/// bookmark (same stack, lower segment_index) are highlighted in red.
/// Non-highlighted commit lines are shown dimmed. A `trunk()` label is
/// appended at the bottom.
fn render_graph(
    term: &Term,
    bookmarks: &[SelectableBookmark],
    focused_index: usize,
) -> io::Result<usize> {
    let focused = &bookmarks[focused_index];
    let focused_stack = focused.stack_index;
    let focused_seg = focused.segment_index;

    let mut lines = 0;
    let mut buf = String::new();

    for (i, bm) in bookmarks.iter().enumerate() {
        // Separator between entries.
        if i > 0 {
            let prev = &bookmarks[i - 1];
            if bm.stack_index == prev.stack_index {
                // Same stack: connector line.
                buf.push_str("  \u{2502}\n"); // │
                lines += 1;
            } else {
                // Different stack: blank line.
                buf.push('\n');
                lines += 1;
            }
        }

        // Determine highlight for this bookmark.
        let is_focused = i == focused_index;
        let is_ancestor = bm.stack_index == focused_stack
            && bm.segment_index < focused_seg;

        // Bookmark line.
        let cursor = if is_focused { "\u{25b6} " } else { "  " }; // ▶ or spaces
        let circle = "\u{25cb}"; // ○
        let name_display = format!("{cursor}{circle} {}", bm.bookmark_name);

        if is_focused {
            buf.push_str(&format!("{}\n", style(name_display).green().bold()));
        } else if is_ancestor {
            buf.push_str(&format!("{}\n", style(name_display).red()));
        } else {
            buf.push_str(&format!("{name_display}\n"));
        }
        lines += 1;

        // Commit description lines.
        for desc in &bm.commit_descriptions {
            let commit_line = format!("  \u{2502}  {desc}"); // │
            if is_focused {
                buf.push_str(&format!("{}\n", style(commit_line).green()));
            } else if is_ancestor {
                buf.push_str(&format!("{}\n", style(commit_line).red()));
            } else {
                buf.push_str(&format!("{}\n", style(commit_line).dim()));
            }
            lines += 1;
        }
    }

    // Trunk label at the bottom, separated from the last stack.
    if !bookmarks.is_empty() {
        buf.push_str("  \u{2502}\n"); // │
        lines += 1;
        buf.push_str(&format!(
            "  {}\n",
            style("\u{25cb} trunk()").dim() // ○ trunk()
        ));
        lines += 1;
    }

    term.write_str(&buf)?;

    Ok(lines)
}

/// Run the interactive bookmark selection loop.
///
/// Returns the index into `bookmarks` that was selected, or
/// `StakkError::PromptCancelled` if the user presses Escape.
fn select_bookmark(
    term: &Term,
    bookmarks: &[SelectableBookmark],
) -> Result<usize, StakkError> {
    let mut focused = bookmarks.len() - 1; // Start at the leaf of the last stack.

    term.hide_cursor().ok();

    // Ensure cursor is restored on any exit path.
    struct CursorGuard<'a>(&'a Term);
    impl Drop for CursorGuard<'_> {
        fn drop(&mut self) {
            self.0.show_cursor().ok();
        }
    }
    let _guard = CursorGuard(term);

    let mut line_count = render_graph(term, bookmarks, focused)?;

    loop {
        match term.read_key() {
            Ok(Key::ArrowUp | Key::Char('k')) => {
                if focused > 0 {
                    focused -= 1;
                } else {
                    focused = bookmarks.len() - 1;
                }
            }
            Ok(Key::ArrowDown | Key::Char('j')) => {
                if focused < bookmarks.len() - 1 {
                    focused += 1;
                } else {
                    focused = 0;
                }
            }
            Ok(Key::Enter) => {
                term.clear_last_lines(line_count)?;
                return Ok(focused);
            }
            Ok(Key::Escape | Key::Char('q')) => {
                term.clear_last_lines(line_count)?;
                return Err(StakkError::PromptCancelled);
            }
            _ => continue,
        }

        term.clear_last_lines(line_count)?;
        line_count = render_graph(term, bookmarks, focused)?;
    }
}

/// Resolve a bookmark interactively from the change graph.
///
/// - No stacks: prints a message, returns `Ok(None)`.
/// - Single bookmark: auto-selects it, prints a message, returns `Ok(Some(name))`.
/// - Multiple bookmarks: shows an interactive graph picker on stderr.
///
/// Returns `StakkError::NotInteractive` if stdin is not a terminal.
/// Returns `StakkError::PromptCancelled` if the user presses Escape.
pub fn resolve_bookmark_interactively(
    graph: &ChangeGraph,
) -> Result<Option<String>, StakkError> {
    let bookmarks = collect_selectable_bookmarks(graph);

    if bookmarks.is_empty() {
        eprintln!("No bookmark stacks found.");
        return Ok(None);
    }

    if bookmarks.len() == 1 {
        let name = bookmarks[0].bookmark_name.clone();
        eprintln!("Auto-selecting the only bookmark: {name}");
        return Ok(Some(name));
    }

    let term = Term::stderr();
    if !term.is_term() {
        return Err(StakkError::NotInteractive);
    }

    eprintln!(
        "Select a bookmark to submit ({}):\n",
        style("arrows/jk to move, Enter to select, Esc/q to cancel").dim(),
    );

    let selected = select_bookmark(&term, &bookmarks)?;
    let name = bookmarks[selected].bookmark_name.clone();

    Ok(Some(name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::BookmarkSegment;
    use crate::graph::types::BranchStack;
    use crate::graph::types::ChangeGraph;
    use crate::graph::types::SegmentCommit;
    use std::collections::HashMap;
    use std::collections::HashSet;

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

    fn make_segment(
        names: &[&str],
        change_id: &str,
        descriptions: &[&str],
    ) -> BookmarkSegment {
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

    #[test]
    fn collect_empty_graph() {
        let graph = make_graph(vec![]);
        let result = collect_selectable_bookmarks(&graph);
        assert!(result.is_empty());
    }

    #[test]
    fn collect_single_bookmark() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(&["feat-a"], "ch_a", &["add feature a"])],
        }]);

        let result = collect_selectable_bookmarks(&graph);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bookmark_name, "feat-a");
        assert_eq!(result[0].stack_index, 0);
        assert_eq!(result[0].segment_index, 0);
        assert_eq!(result[0].stack_len, 1);
    }

    #[test]
    fn collect_multi_segment_stack() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["feat-a"], "ch_a", &["feature a"]),
                make_segment(&["feat-b"], "ch_b", &["feature b"]),
                make_segment(&["feat-c"], "ch_c", &["feature c"]),
            ],
        }]);

        let result = collect_selectable_bookmarks(&graph);
        assert_eq!(result.len(), 3);

        for (i, bm) in result.iter().enumerate() {
            assert_eq!(bm.stack_index, 0);
            assert_eq!(bm.segment_index, i);
            assert_eq!(bm.stack_len, 3);
        }

        assert_eq!(result[0].bookmark_name, "feat-a");
        assert_eq!(result[1].bookmark_name, "feat-b");
        assert_eq!(result[2].bookmark_name, "feat-c");
    }

    #[test]
    fn collect_multiple_stacks() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["alpha"], "ch_alpha", &["alpha"]),
                    make_segment(&["beta"], "ch_beta", &["beta"]),
                ],
            },
            BranchStack {
                segments: vec![make_segment(
                    &["gamma"],
                    "ch_gamma",
                    &["gamma"],
                )],
            },
        ]);

        let result = collect_selectable_bookmarks(&graph);
        assert_eq!(result.len(), 3);

        assert_eq!(result[0].stack_index, 0);
        assert_eq!(result[1].stack_index, 0);
        assert_eq!(result[2].stack_index, 1);

        assert_eq!(result[2].segment_index, 0);
        assert_eq!(result[2].stack_len, 1);
    }

    #[test]
    fn collect_commit_descriptions() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(
                &["feat-a"],
                "ch_a",
                &["first line\n\nmore detail", "second commit\nwith body"],
            )],
        }]);

        let result = collect_selectable_bookmarks(&graph);
        assert_eq!(result[0].commit_descriptions.len(), 2);
        assert_eq!(result[0].commit_descriptions[0], "first line");
        assert_eq!(result[0].commit_descriptions[1], "second commit");
    }

    #[test]
    fn collect_empty_description() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(&["feat-a"], "ch_a", &[""])],
        }]);

        let result = collect_selectable_bookmarks(&graph);
        assert_eq!(result[0].commit_descriptions[0], "(no description)");
    }
}
