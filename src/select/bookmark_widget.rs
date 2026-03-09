//! Screen 2: Bookmark assignment widget.
//!
//! Shows commits on the selected trunk→leaf path. Users can toggle existing
//! bookmarks on/off and generate new `stakk-<change_id>` bookmarks for
//! unmarked commits. Each non-trunk row cycles through three states:
//! `UseExisting` → `UseGenerated` → `Unchecked`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::BookmarkAssignment;
use super::graph_layout::LayoutNode;

/// The inclusion state of a bookmark row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// Included in submission using the existing bookmark.
    UseExisting,
    /// Included in submission; a new stakk-xxx bookmark will be created.
    UseGenerated,
    /// Excluded from submission.
    Unchecked,
}

/// A row in the bookmark assignment view.
#[derive(Debug, Clone)]
pub struct BookmarkRow {
    /// The jj change ID.
    pub change_id: String,
    /// The commit summary.
    pub summary: String,
    /// Existing bookmark name, if any.
    pub existing_bookmark: Option<String>,
    /// Whether and how this row is included in the submission.
    pub state: RowState,
    /// Generated bookmark name (`stakk-<change_id_prefix>`).
    pub generated_name: Option<String>,
    /// Whether this is the trunk row (not toggleable).
    pub is_trunk: bool,
}

impl BookmarkRow {
    /// Get the effective bookmark name for this row.
    #[cfg_attr(not(test), expect(dead_code, reason = "used in tests for validation"))]
    pub fn effective_name(&self) -> Option<&str> {
        if self.is_trunk {
            return None;
        }
        match self.state {
            RowState::UseExisting => self.existing_bookmark.as_deref(),
            RowState::UseGenerated => self.generated_name.as_deref(),
            RowState::Unchecked => None,
        }
    }
}

/// State for the bookmark assignment widget.
#[derive(Debug)]
pub struct BookmarkAssignmentState {
    /// The rows, in trunk-to-leaf order.
    pub rows: Vec<BookmarkRow>,
    /// Currently selected row index.
    pub cursor: usize,
}

impl BookmarkAssignmentState {
    /// Build state from a path of layout nodes (trunk-to-leaf order).
    pub fn from_path(path: &[&LayoutNode]) -> Self {
        let rows: Vec<BookmarkRow> = path
            .iter()
            .map(|node| {
                let existing_bookmark = node.bookmark_names.first().cloned();
                let generated_name = if node.is_trunk {
                    None
                } else {
                    Some(generate_bookmark_name(&node.change_id))
                };
                let state = if existing_bookmark.is_some() {
                    RowState::UseExisting
                } else {
                    RowState::Unchecked
                };

                BookmarkRow {
                    change_id: node.change_id.clone(),
                    summary: node.summary.clone(),
                    existing_bookmark,
                    state,
                    generated_name,
                    is_trunk: node.is_trunk,
                }
            })
            .collect();

        // Start cursor on the first non-trunk row.
        let cursor = rows.iter().position(|r| !r.is_trunk).unwrap_or(0);

        Self { rows, cursor }
    }

    /// Toggle the state of the current row through the three-state cycle.
    ///
    /// - **Existing ≠ generated** (three-state): `UseExisting → UseGenerated →
    ///   Unchecked → UseExisting → …`
    /// - **Existing == generated** (two-state): `UseExisting → Unchecked →
    ///   UseExisting → …`
    /// - **No existing** (two-state): `Unchecked → UseGenerated → Unchecked →
    ///   …`
    pub fn toggle_current(&mut self) {
        let Some(row) = self.rows.get_mut(self.cursor) else {
            return;
        };
        if row.is_trunk {
            return;
        }

        let can_use_generated = match (&row.existing_bookmark, &row.generated_name) {
            (Some(existing), Some(generated)) => existing != generated,
            (None, Some(_)) => true,
            _ => false,
        };

        row.state = match (row.state, can_use_generated) {
            #[expect(
                clippy::match_same_arms,
                reason = "UseExisting+true → UseGenerated is a distinct cycle transition from \
                          Unchecked → UseGenerated"
            )]
            (RowState::UseExisting, true) => RowState::UseGenerated,
            #[expect(
                clippy::match_same_arms,
                reason = "UseExisting+false → Unchecked differs from UseGenerated which always → \
                          Unchecked"
            )]
            (RowState::UseExisting, false) => RowState::Unchecked,
            (RowState::UseGenerated, _) => RowState::Unchecked,
            (RowState::Unchecked, _) if row.existing_bookmark.is_some() => RowState::UseExisting,
            (RowState::Unchecked, _) => RowState::UseGenerated,
        };
    }

    /// Move cursor up (toward leaf = visually up, higher index in rows).
    pub fn cursor_up(&mut self) {
        if self.cursor < self.rows.len().saturating_sub(1) {
            self.cursor += 1;
        }
    }

    /// Move cursor down (toward trunk = visually down, lower index in rows).
    pub fn cursor_down(&mut self) {
        if self.cursor > 0 {
            let next = self.cursor - 1;
            // Don't land on trunk unless it's the only row.
            if self.rows.get(next).is_some_and(|r| r.is_trunk) && self.rows.len() > 1 {
                return;
            }
            self.cursor = next;
        }
    }

    /// Build the selection result from included rows.
    pub fn build_result(&self) -> Vec<BookmarkAssignment> {
        self.rows
            .iter()
            .filter(|r| !r.is_trunk && r.state != RowState::Unchecked)
            .map(|r| {
                let (bookmark_name, is_new) = match r.state {
                    RowState::UseExisting => (
                        r.existing_bookmark
                            .clone()
                            .expect("UseExisting requires name"),
                        false,
                    ),
                    RowState::UseGenerated => (
                        r.generated_name
                            .clone()
                            .expect("UseGenerated requires name"),
                        true,
                    ),
                    RowState::Unchecked => unreachable!("filtered above"),
                };
                BookmarkAssignment {
                    change_id: r.change_id.clone(),
                    bookmark_name,
                    is_new,
                }
            })
            .collect()
    }
}

/// Generate a bookmark name from a change ID.
///
/// Uses `stakk-<first 12 chars of change_id>`, matching jj's `push-<change_id>`
/// convention.
fn generate_bookmark_name(change_id: &str) -> String {
    let prefix = if change_id.len() >= 12 {
        &change_id[..12]
    } else {
        change_id
    };
    format!("stakk-{prefix}")
}

/// Renders the bookmark assignment screen.
pub struct BookmarkWidget<'a> {
    state: &'a BookmarkAssignmentState,
}

impl<'a> BookmarkWidget<'a> {
    pub fn new(state: &'a BookmarkAssignmentState) -> Self {
        Self { state }
    }

    fn build_lines(&self) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        // Render rows in reverse (leaf at top, trunk at bottom).
        for (idx, row) in self.state.rows.iter().enumerate().rev() {
            let is_selected = idx == self.state.cursor;

            if row.is_trunk {
                let style = Style::default().fg(Color::DarkGray);
                lines.push(Line::from(vec![
                    Span::styled("      ", style),
                    Span::styled("\u{25c6} ", style), // ◆
                    Span::styled("trunk", style),
                ]));
                continue;
            }

            let node_char = "\u{25cb}"; // ○
            let cursor_indicator = if is_selected { "> " } else { "  " };

            // Per-state checkbox symbol and color.
            let (checkbox, state_color, state_bold) = match row.state {
                RowState::UseExisting => ("[x]", Color::Green, true),
                RowState::UseGenerated => ("[+]", Color::Yellow, true),
                RowState::Unchecked => ("[ ]", Color::DarkGray, false),
            };

            let name_str = match row.state {
                RowState::UseExisting => row.existing_bookmark.clone().unwrap_or_default(),
                RowState::UseGenerated => row
                    .generated_name
                    .as_ref()
                    .map(|n| format!("{n} (new)"))
                    .unwrap_or_default(),
                RowState::Unchecked => {
                    if let Some(ref existing) = row.existing_bookmark {
                        existing.clone()
                    } else if let Some(ref gen_name) = row.generated_name {
                        format!("{gen_name} (Space to create)")
                    } else {
                        String::new()
                    }
                }
            };

            let cursor_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let state_style = {
                let base = Style::default().fg(state_color);
                if state_bold {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                }
            };

            let summary_style = if is_selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mut spans = vec![
                Span::styled(cursor_indicator.to_string(), cursor_style),
                Span::styled(format!("{checkbox} "), state_style),
                Span::styled(format!("{node_char} "), state_style),
            ];

            if !name_str.is_empty() {
                spans.push(Span::styled(format!("{name_str}  "), state_style));
            }

            spans.push(Span::styled(row.summary.clone(), summary_style));

            lines.push(Line::from(spans));
        }

        lines
    }
}

impl Widget for BookmarkWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.build_lines();

        for (i, line) in lines.iter().take(area.height as usize).enumerate() {
            let y = area.y + u16::try_from(i).expect("line index fits in u16");
            buf.set_line(area.x, y, line, area.width);
        }
    }
}

/// Build a help line for the bottom of the bookmark view.
pub fn bookmark_help_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " \u{2191}\u{2193}/jk",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" navigate  "),
        Span::styled(
            "Space",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" [x]use \u{2192} [+]new \u{2192} [ ]skip  "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" confirm  "),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" back"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::graph_layout::LayoutNode;

    fn make_node(
        change_id: &str,
        summary: &str,
        bookmarks: &[&str],
        is_trunk: bool,
        is_leaf: bool,
    ) -> LayoutNode {
        LayoutNode {
            row: 0,
            col: 0,
            change_id: change_id.to_string(),
            commit_id: format!("commit_{change_id}"),
            summary: summary.to_string(),
            bookmark_names: bookmarks.iter().map(ToString::to_string).collect(),
            is_trunk,
            is_leaf,
            stack_index: 0,
        }
    }

    #[test]
    fn generate_name_from_change_id() {
        assert_eq!(
            generate_bookmark_name("abcdefghijklmnop"),
            "stakk-abcdefghijkl"
        );
        assert_eq!(generate_bookmark_name("short"), "stakk-short");
    }

    #[test]
    fn state_from_path_marks_existing_bookmarks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "add base", &["base"], false, false),
            make_node("ch_b", "add feature", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let state = BookmarkAssignmentState::from_path(&refs);

        assert_eq!(state.rows.len(), 3);

        // Trunk is not toggleable.
        assert!(state.rows[0].is_trunk);

        // Base has existing bookmark → UseExisting; generated_name is always set now.
        assert_eq!(state.rows[1].state, RowState::UseExisting);
        assert_eq!(state.rows[1].existing_bookmark, Some("base".to_string()));
        assert_eq!(state.rows[1].generated_name, Some("stakk-ch_a".to_string()));

        // Unmarked commit has generated name, Unchecked by default.
        assert_eq!(state.rows[2].state, RowState::Unchecked);
        assert!(state.rows[2].existing_bookmark.is_none());
        assert!(state.rows[2].generated_name.is_some());
    }

    #[test]
    fn toggle_checks_and_unchecks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "work", &["feat"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);

        // Cursor should start on the non-trunk row; starts UseExisting.
        assert_eq!(state.cursor, 1);
        assert_eq!(state.rows[1].state, RowState::UseExisting);

        // "feat" != "stakk-ch_a" → three-state cycle.
        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseGenerated);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting);
    }

    #[test]
    fn toggle_trunk_is_noop() {
        let nodes = [make_node("", "trunk", &[], true, false)];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);
        state.cursor = 0;
        let state_before = state.rows[0].state;
        state.toggle_current();
        assert_eq!(state.rows[0].state, state_before);
    }

    #[test]
    fn toggle_two_state_when_names_match() {
        // change_id "abcdefghijkl" (12 chars) → generated "stakk-abcdefghijkl"
        // existing bookmark matches generated → two-state: UseExisting ↔ Unchecked
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("abcdefghijkl", "work", &["stakk-abcdefghijkl"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);

        assert_eq!(state.rows[1].state, RowState::UseExisting);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting);
    }

    #[test]
    fn toggle_no_existing_two_state() {
        // No existing bookmark → two-state: Unchecked ↔ UseGenerated
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_x", "feature", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);

        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseGenerated);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);
    }

    #[test]
    fn build_result_includes_only_checked() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "base", &["base"], false, false),
            make_node("ch_b", "middle", &[], false, false),
            make_node("ch_c", "leaf", &["leaf"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);

        // Toggle the middle (unmarked) commit: Unchecked → UseGenerated.
        state.cursor = 2;
        state.toggle_current();

        let result = state.build_result();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].bookmark_name, "base");
        assert!(!result[0].is_new);
        assert!(result[1].bookmark_name.starts_with("stakk-"));
        assert!(result[1].is_new);
        assert_eq!(result[2].bookmark_name, "leaf");
        assert!(!result[2].is_new);
    }

    #[test]
    fn build_result_empty_when_all_unchecked() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "work", &["feat"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs);

        // Toggle twice: UseExisting → UseGenerated → Unchecked.
        state.cursor = 1;
        state.toggle_current();
        state.toggle_current();

        let result = state.build_result();
        assert!(result.is_empty());
    }

    #[test]
    fn effective_name_returns_correct_values() {
        let row_existing = BookmarkRow {
            change_id: "a".to_string(),
            summary: "work".to_string(),
            existing_bookmark: Some("feat".to_string()),
            state: RowState::UseExisting,
            generated_name: None,
            is_trunk: false,
        };
        assert_eq!(row_existing.effective_name(), Some("feat"));

        let row_generated = BookmarkRow {
            change_id: "b".to_string(),
            summary: "work".to_string(),
            existing_bookmark: None,
            state: RowState::UseGenerated,
            generated_name: Some("stakk-bbbbbbbbb".to_string()),
            is_trunk: false,
        };
        assert_eq!(row_generated.effective_name(), Some("stakk-bbbbbbbbb"));

        let row_unchecked = BookmarkRow {
            change_id: "c".to_string(),
            summary: "work".to_string(),
            existing_bookmark: Some("feat".to_string()),
            state: RowState::Unchecked,
            generated_name: None,
            is_trunk: false,
        };
        assert_eq!(row_unchecked.effective_name(), None);
    }

    #[test]
    fn bookmark_widget_renders_to_buffer() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "add feature", &["feat"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let state = BookmarkAssignmentState::from_path(&refs);
        let widget = BookmarkWidget::new(&state);

        let area = Rect::new(0, 0, 60, 10);
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

        assert!(content.contains("[x]"), "expected checkbox in output");
        assert!(content.contains("feat"), "expected bookmark name in output");
    }
}
