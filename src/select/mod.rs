//! Interactive bookmark selection using a ratatui TUI.
//!
//! Two screens: a graph view for selecting a branch path, then a bookmark
//! assignment view for toggling which commits get bookmarks.

mod app;
pub(crate) mod bookmark_gen;
mod bookmark_widget;
mod event;
mod graph_layout;
mod graph_widget;
mod tfidf;

use std::io::IsTerminal;

use crate::error::StakkError;
use crate::graph::types::ChangeGraph;

/// A bookmark assignment for a commit in the submission stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkAssignment {
    /// The jj change ID for this commit.
    pub change_id: String,
    /// The bookmark name (existing or newly generated).
    pub bookmark_name: String,
    /// `true` if stakk must run `jj bookmark create` for this bookmark.
    pub is_new: bool,
}

/// Result of the interactive selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionResult {
    /// Ordered trunk-to-leaf bookmark assignments.
    pub assignments: Vec<BookmarkAssignment>,
}

/// Resolve bookmarks interactively from the change graph using a TUI.
///
/// - No stacks: prints a message, returns `Ok(None)`.
/// - Otherwise: shows the TUI graph view, then bookmark assignment.
///
/// Returns `StakkError::NotInteractive` if stdin is not a terminal.
/// Returns `StakkError::PromptCancelled` if the user presses Escape/q.
pub fn resolve_bookmark_interactively(
    graph: &ChangeGraph,
    bookmark_command: Option<&str>,
    auto_prefix: Option<&str>,
) -> Result<Option<SelectionResult>, StakkError> {
    if graph.stacks.is_empty() {
        eprintln!("No bookmark stacks found.");
        return Ok(None);
    }

    if !std::io::stdin().is_terminal() {
        return Err(StakkError::NotInteractive);
    }

    app::run_tui(graph, bookmark_command, auto_prefix)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use super::*;
    use crate::graph::types::ChangeGraph;

    fn make_graph_empty() -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: HashSet::new(),
            stack_roots: HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: HashSet::new(),
            excluded_bookmark_count: 0,
            stacks: vec![],
        }
    }

    #[test]
    fn resolve_no_stacks() {
        let graph = make_graph_empty();
        let result = resolve_bookmark_interactively(&graph, None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn bookmark_assignment_new_flag() {
        let existing = BookmarkAssignment {
            change_id: "abc123".to_string(),
            bookmark_name: "my-feature".to_string(),
            is_new: false,
        };
        assert!(!existing.is_new);

        let generated = BookmarkAssignment {
            change_id: "def456".to_string(),
            bookmark_name: "stakk-def456abcdef".to_string(),
            is_new: true,
        };
        assert!(generated.is_new);
    }

    #[test]
    fn selection_result_ordering() {
        let result = SelectionResult {
            assignments: vec![
                BookmarkAssignment {
                    change_id: "base".to_string(),
                    bookmark_name: "base-bm".to_string(),
                    is_new: false,
                },
                BookmarkAssignment {
                    change_id: "leaf".to_string(),
                    bookmark_name: "leaf-bm".to_string(),
                    is_new: true,
                },
            ],
        };
        assert_eq!(result.assignments.len(), 2);
        assert_eq!(result.assignments[0].bookmark_name, "base-bm");
        assert_eq!(result.assignments[1].bookmark_name, "leaf-bm");
    }
}
