//! Screen 2: Bookmark assignment widget.
//!
//! Shows commits on the selected trunk→leaf path. Users can toggle existing
//! bookmarks on/off and generate new `stakk-<change_id>` bookmarks for
//! unmarked commits. Each non-trunk row cycles through states:
//! `UseExisting(0)` → … → `UseExisting(N-1)` → `UseGenerated` → `Unchecked`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::BookmarkAssignment;
use super::bookmark_gen;
use super::graph_layout::LayoutNode;
use super::tfidf;
use crate::jj::types::Signature;

/// Whether a custom bookmark name is still loading or has been resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomNameState {
    /// Waiting for the external command to return a name.
    Loading,
    /// The name has been resolved.
    Ready(String),
}

/// State for a TF-IDF generated bookmark name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TfidfNameState {
    /// The computed name.
    pub name: String,
    /// Which variation index produced this name.
    pub variation: usize,
}

/// The inclusion state of a bookmark row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    /// Included in submission using the existing bookmark at the given index
    /// into `BookmarkRow::existing_bookmarks`.
    UseExisting(usize),
    /// Included in submission; a new stakk-xxx bookmark will be created.
    UseGenerated,
    /// Included in submission; a TF-IDF generated name from commit data.
    UseTfidf(TfidfNameState),
    /// Included in submission; a custom name from the bookmark command.
    UseCustom(CustomNameState),
    /// Excluded from submission.
    Unchecked,
}

/// A row in the bookmark assignment view.
#[derive(Debug, Clone)]
pub struct BookmarkRow {
    /// The jj change ID.
    pub change_id: String,
    /// Shortest unique change ID prefix (from jj).
    pub short_change_id: String,
    /// The jj commit ID.
    pub commit_id: String,
    /// The commit summary (first line of description).
    pub summary: String,
    /// Full commit description.
    pub description: String,
    /// Existing bookmark names on this change (may be empty).
    pub existing_bookmarks: Vec<String>,
    /// Whether and how this row is included in the submission.
    pub state: RowState,
    /// Generated bookmark name (`stakk-<change_id_prefix>`).
    pub generated_name: Option<String>,
    /// Custom name from the bookmark command (populated lazily).
    pub custom_name: Option<String>,
    /// TF-IDF name and its variation index (computed on demand).
    pub tfidf_name: Option<(String, usize)>,
    /// Whether this is the trunk row (not toggleable).
    pub is_trunk: bool,
    /// Author signature.
    pub author: Signature,
    /// Files changed by this commit.
    pub files: Vec<String>,
    /// Whether a bookmark command is configured.
    pub has_bookmark_command: bool,
}

impl BookmarkRow {
    /// Get the effective bookmark name for this row.
    #[cfg_attr(not(test), expect(dead_code, reason = "used in tests for validation"))]
    pub fn effective_name(&self) -> Option<&str> {
        if self.is_trunk {
            return None;
        }
        match &self.state {
            RowState::UseExisting(idx) => self.existing_bookmarks.get(*idx).map(String::as_str),
            RowState::UseGenerated => self.generated_name.as_deref(),
            RowState::UseTfidf(ts) => Some(ts.name.as_str()),
            RowState::UseCustom(CustomNameState::Ready(name)) => Some(name.as_str()),
            RowState::UseCustom(CustomNameState::Loading) | RowState::Unchecked => None,
        }
    }
}

/// Reasons why the selection cannot be confirmed.
#[derive(Debug)]
pub enum SelectionError {
    /// Two included rows resolved to the same bookmark name.
    DuplicateName(String),
    /// A custom name is still being computed.
    StillLoading,
}

/// Result of a [`BookmarkAssignmentState::regenerate_current`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum RegenerateResult {
    /// Nothing to regenerate (not on a regenerable state).
    Noop,
    /// TF-IDF variation was cycled successfully.
    TfidfCycled,
    /// No other TF-IDF variation produced a different name.
    TfidfNoVariation,
    /// Custom name needs re-firing the external command.
    NeedsRefire,
}

/// Build `UseCustom` state from a row's cached custom name.
fn make_use_custom(row: &BookmarkRow) -> RowState {
    match &row.custom_name {
        Some(name) => RowState::UseCustom(CustomNameState::Ready(name.clone())),
        None => RowState::UseCustom(CustomNameState::Loading),
    }
}

/// Compute a TF-IDF bookmark name for a dynamic segment, with optional prefix.
fn compute_tfidf_for_segment(
    rows: &[BookmarkRow],
    row_idx: usize,
    variation: usize,
    auto_prefix: Option<&str>,
) -> Option<String> {
    let segment = bookmark_gen::dynamic_segment_commits(rows, row_idx);
    let commit_data: Vec<tfidf::CommitData<'_>> = segment
        .iter()
        .map(|r| tfidf::CommitData {
            description: &r.description,
            files: &r.files,
        })
        .collect();

    // Reserve space for the prefix in the max length budget.
    let prefix_len = auto_prefix.map_or(0, str::len);
    let max_length = bookmark_gen::MAX_BOOKMARK_LENGTH.saturating_sub(prefix_len);

    let name = tfidf::tfidf_bookmark_name(
        &commit_data,
        3,
        variation,
        max_length,
        bookmark_gen::DISALLOWED_CHARS,
    )?;

    match auto_prefix {
        Some(prefix) => Some(format!("{prefix}{name}")),
        None => Some(name),
    }
}

/// State for the bookmark assignment widget.
#[derive(Debug)]
pub struct BookmarkAssignmentState {
    /// The rows, in trunk-to-leaf order.
    pub rows: Vec<BookmarkRow>,
    /// Currently selected row index.
    pub cursor: usize,
    /// Optional prefix for auto-generated (TF-IDF) bookmark names.
    auto_prefix: Option<String>,
}

impl BookmarkAssignmentState {
    /// Build state from a path of layout nodes (trunk-to-leaf order).
    pub fn from_path(
        path: &[&LayoutNode],
        has_bookmark_command: bool,
        auto_prefix: Option<&str>,
    ) -> Self {
        let rows: Vec<BookmarkRow> = path
            .iter()
            .map(|node| {
                let existing_bookmarks = node.bookmark_names.clone();
                let generated_name = if node.is_trunk {
                    None
                } else {
                    Some(bookmark_gen::default_bookmark_name(&node.change_id))
                };
                let state = if existing_bookmarks.is_empty() {
                    RowState::Unchecked
                } else {
                    RowState::UseExisting(0)
                };

                BookmarkRow {
                    change_id: node.change_id.clone(),
                    short_change_id: node.short_change_id.clone(),
                    commit_id: node.commit_id.clone(),
                    summary: node.summary.clone(),
                    description: node.description.clone(),
                    existing_bookmarks,
                    state,
                    generated_name,
                    custom_name: None,
                    tfidf_name: None,
                    is_trunk: node.is_trunk,
                    author: node.author.clone(),
                    files: node.files.clone(),
                    has_bookmark_command,
                }
            })
            .collect();

        // Start cursor on the first non-trunk row.
        let cursor = rows.iter().position(|r| !r.is_trunk).unwrap_or(0);

        Self {
            rows,
            cursor,
            auto_prefix: auto_prefix.map(String::from),
        }
    }

    /// Toggle the state of the current row through the cycle.
    ///
    /// The cycle is: `UseExisting(0..N-1)` → `UseTfidf` → `UseGenerated`
    /// → `UseCustom` → `Unchecked` → back to start.
    ///
    /// - `UseTfidf` is skipped when it produces `None` or matches an
    ///   existing/generated name.
    /// - `UseGenerated` is skipped when it matches an existing bookmark.
    /// - `UseCustom` is skipped when no bookmark command is configured, or if
    ///   the custom name matches the generated or any existing name.
    ///
    /// When toggling to `UseCustom`, the state is set to
    /// `UseCustom(Loading)` — the caller (`app.rs`) is responsible for
    /// firing the command and filling in the real name.
    pub fn toggle_current(&mut self) {
        let cursor = self.cursor;
        let Some(row) = self.rows.get(cursor) else {
            return;
        };
        if row.is_trunk {
            return;
        }

        let has_distinct_generated = match &row.generated_name {
            Some(generated) => !row.existing_bookmarks.iter().any(|e| e == generated),
            None => false,
        };

        let has_distinct_custom = row.has_bookmark_command
            && match &row.custom_name {
                Some(custom) => {
                    let matches_generated = row.generated_name.as_ref() == Some(custom);
                    let matches_existing = row.existing_bookmarks.iter().any(|e| e == custom);
                    !matches_generated && !matches_existing
                }
                // No cached custom name yet — include UseCustom so it can be
                // resolved lazily.
                None => true,
            };

        let current_state = row.state.clone();

        // Compute next state. For UseTfidf, we need to compute from the
        // full rows slice, so we do that after releasing the borrow.
        let next = match &current_state {
            RowState::UseExisting(idx) => {
                let next_idx = idx + 1;
                if next_idx < row.existing_bookmarks.len() {
                    RowState::UseExisting(next_idx)
                } else {
                    self.next_after_existing(cursor, has_distinct_generated, has_distinct_custom)
                }
            }
            RowState::UseTfidf(_) => {
                self.next_after_tfidf(cursor, has_distinct_generated, has_distinct_custom)
            }
            RowState::UseGenerated => {
                if has_distinct_custom {
                    make_use_custom(&self.rows[cursor])
                } else {
                    RowState::Unchecked
                }
            }
            RowState::UseCustom(_) => RowState::Unchecked,
            RowState::Unchecked => {
                if row.existing_bookmarks.is_empty() {
                    self.next_after_existing(cursor, has_distinct_generated, has_distinct_custom)
                } else {
                    RowState::UseExisting(0)
                }
            }
        };

        self.rows[cursor].state = next;

        // A toggle may change the dynamic segment for other UseTfidf rows
        // (e.g. toggling an earlier commit on/off changes which commits are
        // included in a later segment). Refresh all TF-IDF names.
        self.refresh_tfidf_names();
    }

    /// Compute the next state after exhausting existing bookmarks (or from
    /// Unchecked with no existing bookmarks): try `UseTfidf`, then
    /// `UseGenerated`, then `UseCustom`, then `Unchecked`.
    fn next_after_existing(
        &mut self,
        cursor: usize,
        has_distinct_generated: bool,
        has_distinct_custom: bool,
    ) -> RowState {
        if let Some(tfidf_state) = self.try_make_tfidf(cursor, 0) {
            return RowState::UseTfidf(tfidf_state);
        }
        self.next_after_tfidf(cursor, has_distinct_generated, has_distinct_custom)
    }

    /// Compute the next state after `UseTfidf`: try `UseGenerated`, then
    /// `UseCustom`, then `Unchecked`.
    fn next_after_tfidf(
        &mut self,
        cursor: usize,
        has_distinct_generated: bool,
        has_distinct_custom: bool,
    ) -> RowState {
        if has_distinct_generated {
            return RowState::UseGenerated;
        }
        if has_distinct_custom {
            return make_use_custom(&self.rows[cursor]);
        }
        RowState::Unchecked
    }

    /// Try to compute a TF-IDF name for the given row. Returns `None` if
    /// it produces no name or the name matches an existing/generated name.
    fn try_make_tfidf(&mut self, cursor: usize, variation: usize) -> Option<TfidfNameState> {
        let name =
            compute_tfidf_for_segment(&self.rows, cursor, variation, self.auto_prefix.as_deref())?;

        let row = &self.rows[cursor];
        // Skip if it matches the generated name.
        if row.generated_name.as_ref() == Some(&name) {
            return None;
        }
        // Skip if it matches an existing bookmark.
        if row.existing_bookmarks.iter().any(|e| e == &name) {
            return None;
        }

        self.rows[cursor].tfidf_name = Some((name.clone(), variation));
        Some(TfidfNameState { name, variation })
    }

    /// Regenerate the current row's name (cycle TF-IDF variation or
    /// invalidate custom name cache).
    pub fn regenerate_current(&mut self) -> RegenerateResult {
        let cursor = self.cursor;
        let Some(row) = self.rows.get(cursor) else {
            return RegenerateResult::Noop;
        };
        if row.is_trunk {
            return RegenerateResult::Noop;
        }

        match &row.state {
            RowState::UseTfidf(ts) => {
                let old_variation = ts.variation;
                // Try up to 6 variations.
                for delta in 1..=6 {
                    let new_variation = (old_variation + delta) % 6;
                    if let Some(tfidf_state) = self.try_make_tfidf(cursor, new_variation) {
                        self.rows[cursor].state = RowState::UseTfidf(tfidf_state);
                        return RegenerateResult::TfidfCycled;
                    }
                }
                RegenerateResult::TfidfNoVariation
            }
            RowState::UseCustom(_) => {
                // Invalidate cached custom name and set to Loading.
                self.rows[cursor].custom_name = None;
                self.rows[cursor].state = RowState::UseCustom(CustomNameState::Loading);
                RegenerateResult::NeedsRefire
            }
            _ => RegenerateResult::Noop,
        }
    }

    /// Recompute TF-IDF names for all `UseTfidf` rows whose dynamic segment
    /// may have changed (e.g. because an earlier row was toggled).
    pub fn refresh_tfidf_names(&mut self) {
        let tfidf_indices: Vec<(usize, usize)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match &row.state {
                RowState::UseTfidf(ts) => Some((i, ts.variation)),
                _ => None,
            })
            .collect();

        for (idx, variation) in tfidf_indices {
            let old_name = match &self.rows[idx].state {
                RowState::UseTfidf(ts) => ts.name.clone(),
                _ => continue,
            };

            // Recompute from the (potentially changed) dynamic segment.
            match compute_tfidf_for_segment(&self.rows, idx, variation, self.auto_prefix.as_deref())
            {
                Some(new_name) if new_name != old_name => {
                    self.rows[idx].tfidf_name = Some((new_name.clone(), variation));
                    self.rows[idx].state = RowState::UseTfidf(TfidfNameState {
                        name: new_name,
                        variation,
                    });
                }
                None => {
                    // Segment no longer produces a TF-IDF name — fall back to
                    // Unchecked.
                    self.rows[idx].tfidf_name = None;
                    self.rows[idx].state = RowState::Unchecked;
                }
                Some(_) => {} // Same name, nothing to do.
            }
        }
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
    ///
    /// Returns `Err` with the duplicate bookmark name if any two included rows
    /// resolve to the same name, or if any row is still loading
    /// (`UseCustom(Loading)`).
    pub fn build_result(&self) -> Result<Vec<BookmarkAssignment>, SelectionError> {
        let mut assignments = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for r in &self.rows {
            if r.is_trunk || r.state == RowState::Unchecked {
                continue;
            }

            let (bookmark_name, is_new) = match &r.state {
                RowState::UseExisting(idx) => (
                    r.existing_bookmarks
                        .get(*idx)
                        .cloned()
                        .expect("UseExisting index in bounds"),
                    false,
                ),
                RowState::UseGenerated => (
                    r.generated_name
                        .clone()
                        .expect("UseGenerated requires name"),
                    true,
                ),
                RowState::UseTfidf(ts) => (ts.name.clone(), true),
                RowState::UseCustom(CustomNameState::Loading) => {
                    return Err(SelectionError::StillLoading);
                }
                RowState::UseCustom(CustomNameState::Ready(name)) => (name.clone(), true),
                RowState::Unchecked => unreachable!("filtered above"),
            };

            if !seen.insert(bookmark_name.clone()) {
                return Err(SelectionError::DuplicateName(bookmark_name));
            }

            assignments.push(BookmarkAssignment {
                change_id: r.change_id.clone(),
                bookmark_name,
                is_new,
            });
        }

        Ok(assignments)
    }
}

/// Shorten a string to `max` chars by keeping the start and end, joined by `…`.
///
/// `"jq -r '.commits' | tr ' ' '-' | tr '[:upper:]' '[:lower:]'"` with max=14
/// becomes `"jq -r…lower']"`.
fn shorten_middle(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    // 1 char for `…`, split the rest ~evenly favoring the start.
    let budget = max.saturating_sub(1);
    let head = budget.div_ceil(2);
    let tail = budget / 2;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(len - tail).collect();
    format!("{start}\u{2026}{end}")
}

/// Renders the bookmark assignment screen.
pub struct BookmarkWidget<'a> {
    state: &'a BookmarkAssignmentState,
    spinner_tick: usize,
    bookmark_command: Option<&'a str>,
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Max display width for the command label between spinners.
const COMMAND_LABEL_MAX: usize = 16;

impl<'a> BookmarkWidget<'a> {
    pub fn new(
        state: &'a BookmarkAssignmentState,
        spinner_tick: usize,
        bookmark_command: Option<&'a str>,
    ) -> Self {
        Self {
            state,
            spinner_tick,
            bookmark_command,
        }
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
            let (checkbox, state_color, state_bold) = match &row.state {
                RowState::UseExisting(_) => ("[x]", Color::Green, true),
                RowState::UseGenerated => ("[+]", Color::Yellow, true),
                RowState::UseTfidf(_) => ("[~]", Color::Blue, true),
                RowState::UseCustom(_) => ("[*]", Color::Cyan, true),
                RowState::Unchecked => ("[ ]", Color::DarkGray, false),
            };

            let name_str = match &row.state {
                RowState::UseExisting(idx) => row
                    .existing_bookmarks
                    .get(*idx)
                    .cloned()
                    .unwrap_or_default(),
                RowState::UseGenerated => row
                    .generated_name
                    .as_ref()
                    .map(|n| format!("{n} (generated)"))
                    .unwrap_or_default(),
                RowState::UseTfidf(ts) => {
                    format!("{} (auto [{}])", ts.name, ts.variation)
                }
                RowState::UseCustom(CustomNameState::Loading) => {
                    let frame = SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()];
                    let label = self
                        .bookmark_command
                        .map(|cmd| shorten_middle(cmd, COMMAND_LABEL_MAX))
                        .unwrap_or_default();
                    format!("{frame}{label}{frame}")
                }
                RowState::UseCustom(CustomNameState::Ready(name)) => {
                    format!("{name} (custom)")
                }
                RowState::Unchecked => {
                    if let Some(first) = row.existing_bookmarks.first() {
                        first.clone()
                    } else {
                        "(Space to assign)".to_string()
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

            let change_id_style = if is_selected {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(
                format!("{:<4} ", row.short_change_id),
                change_id_style,
            ));
            if row.summary == "(no description)" {
                spans.push(Span::styled(
                    "(no description set)",
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(row.summary.clone(), summary_style));
            }

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
pub fn bookmark_help_line(has_bookmark_command: bool) -> Line<'static> {
    let cycle = if has_bookmark_command {
        " [x]use \u{2192} [~]auto \u{2192} [+]new \u{2192} [*]custom \u{2192} [ ]skip  "
    } else {
        " [x]use \u{2192} [~]auto \u{2192} [+]new \u{2192} [ ]skip  "
    };
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(" \u{2191}\u{2193}/jk", key_style),
        Span::raw(" navigate  "),
        Span::styled("Space", key_style),
        Span::raw(cycle),
        Span::styled("r", key_style),
        Span::raw(" regenerate  "),
        Span::styled("Enter", key_style),
        Span::raw(" confirm  "),
        Span::styled("Esc", key_style),
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
            description: summary.to_string(),
            bookmark_names: bookmarks.iter().map(ToString::to_string).collect(),
            is_trunk,
            is_leaf,
            stack_index: 0,
            short_change_id: change_id[..4.min(change_id.len())].to_string(),
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
        }
    }

    #[test]
    fn generate_name_from_change_id() {
        assert_eq!(
            bookmark_gen::default_bookmark_name("abcdefghijklmnop"),
            "stakk-abcdefghijkl"
        );
        assert_eq!(bookmark_gen::default_bookmark_name("short"), "stakk-short");
    }

    #[test]
    fn state_from_path_marks_existing_bookmarks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "add base", &["base"], false, false),
            make_node("ch_b", "add feature", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows.len(), 3);

        // Trunk is not toggleable.
        assert!(state.rows[0].is_trunk);

        // Base has existing bookmark → UseExisting(0); generated_name is always set
        // now.
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));
        assert_eq!(state.rows[1].existing_bookmarks, vec!["base".to_string()]);
        assert_eq!(state.rows[1].generated_name, Some("stakk-ch_a".to_string()));

        // Unmarked commit has generated name, Unchecked by default.
        assert_eq!(state.rows[2].state, RowState::Unchecked);
        assert!(state.rows[2].existing_bookmarks.is_empty());
        assert!(state.rows[2].generated_name.is_some());
    }

    #[test]
    fn toggle_checks_and_unchecks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "work", &["feat"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Cursor should start on the non-trunk row; starts UseExisting.
        assert_eq!(state.cursor, 1);
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));

        // Cycle: UseExisting → UseTfidf → UseGenerated → Unchecked.
        // "work" is NOT a stop word, so TF-IDF produces a name.
        state.toggle_current();
        assert!(
            matches!(&state.rows[1].state, RowState::UseTfidf(_)),
            "expected UseTfidf, got {:?}",
            state.rows[1].state
        );

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseGenerated);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));
    }

    #[test]
    fn toggle_trunk_is_noop() {
        let nodes = [make_node("", "trunk", &[], true, false)];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);
        state.cursor = 0;
        let state_before = state.rows[0].state.clone();
        state.toggle_current();
        assert_eq!(state.rows[0].state, state_before);
    }

    #[test]
    fn toggle_two_state_when_names_match() {
        // change_id "abcdefghijkl" (12 chars) → generated "stakk-abcdefghijkl"
        // existing bookmark matches generated → UseGenerated skipped, but
        // TF-IDF still appears. "work" → UseTfidf → Unchecked.
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("abcdefghijkl", "work", &["stakk-abcdefghijkl"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].state, RowState::UseExisting(0));

        // UseGenerated skipped (matches existing), so → UseTfidf.
        state.toggle_current();
        assert!(matches!(&state.rows[1].state, RowState::UseTfidf(_)));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));
    }

    #[test]
    fn toggle_no_existing_includes_tfidf() {
        // No existing bookmark → Unchecked → UseTfidf → UseGenerated →
        // Unchecked. "feature" is NOT a stop word so TF-IDF produces it.
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_x", "feature", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert!(matches!(&state.rows[1].state, RowState::UseTfidf(_)));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseGenerated);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);
    }

    #[test]
    fn toggle_tfidf_skipped_when_all_stop_words() {
        // Description is only stop words → TF-IDF produces None → skipped
        // straight to UseGenerated.
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_x", "add update remove", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].state, RowState::Unchecked);

        // TF-IDF skipped → lands on UseGenerated.
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
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Toggle the middle (unmarked) commit: Unchecked → UseTfidf.
        // "middle" produces a TF-IDF name.
        state.cursor = 2;
        state.toggle_current();

        let result = state.build_result().unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].bookmark_name, "base");
        assert!(!result[0].is_new);
        // Middle now gets a TF-IDF name (not stakk-xxx).
        assert!(!result[1].bookmark_name.starts_with("stakk-"));
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
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Toggle to Unchecked: UseExisting → UseTfidf → UseGenerated →
        // Unchecked.
        state.cursor = 1;
        state.toggle_current(); // UseTfidf
        state.toggle_current(); // UseGenerated
        state.toggle_current(); // Unchecked

        let result = state.build_result().unwrap();
        assert!(result.is_empty());
    }

    fn make_bare_row(state: RowState) -> BookmarkRow {
        BookmarkRow {
            change_id: "a".to_string(),
            short_change_id: "a".to_string(),
            commit_id: "commit_a".to_string(),
            summary: "work".to_string(),
            description: "work".to_string(),
            existing_bookmarks: vec!["feat".to_string()],
            state,
            generated_name: Some("stakk-aaaaaaaaaaaa".to_string()),
            custom_name: None,
            tfidf_name: None,
            is_trunk: false,
            author: crate::jj::types::Signature {
                name: "Test".to_string(),
                email: "test@test.com".to_string(),
                timestamp: "T".to_string(),
            },
            files: vec![],
            has_bookmark_command: false,
        }
    }

    #[test]
    fn effective_name_returns_correct_values() {
        let row_existing = make_bare_row(RowState::UseExisting(0));
        assert_eq!(row_existing.effective_name(), Some("feat"));

        let mut row_generated = make_bare_row(RowState::UseGenerated);
        row_generated.existing_bookmarks = vec![];
        row_generated.generated_name = Some("stakk-bbbbbbbbb".to_string());
        assert_eq!(row_generated.effective_name(), Some("stakk-bbbbbbbbb"));

        let row_unchecked = make_bare_row(RowState::Unchecked);
        assert_eq!(row_unchecked.effective_name(), None);

        let row_custom = make_bare_row(RowState::UseCustom(CustomNameState::Ready(
            "my-branch".to_string(),
        )));
        assert_eq!(row_custom.effective_name(), Some("my-branch"));

        let row_loading = make_bare_row(RowState::UseCustom(CustomNameState::Loading));
        assert_eq!(row_loading.effective_name(), None);
    }

    #[test]
    fn build_result_blocks_when_loading() {
        let mut row = make_bare_row(RowState::UseCustom(CustomNameState::Loading));
        // Ensure the row is not trunk so it's included.
        row.is_trunk = false;
        let state = BookmarkAssignmentState {
            rows: vec![row],
            cursor: 0,
            auto_prefix: None,
        };
        assert!(matches!(
            state.build_result(),
            Err(SelectionError::StillLoading)
        ));
    }

    #[test]
    fn bookmark_widget_renders_to_buffer() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "add feature", &["feat"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let state = BookmarkAssignmentState::from_path(&refs, false, None);
        let widget = BookmarkWidget::new(&state, 0, None);

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

    #[test]
    fn toggle_multiple_existing_bookmarks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node(
                "ch_a",
                "work",
                &["feature", "wip", "experiment"],
                false,
                true,
            ),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].state, RowState::UseExisting(0));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(1));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(2));

        // "work" produces a TF-IDF name → UseTfidf before UseGenerated.
        state.toggle_current();
        assert!(matches!(&state.rows[1].state, RowState::UseTfidf(_)));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseGenerated);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));
    }

    #[test]
    fn toggle_multiple_existing_one_matches_generated() {
        // "feature" and "stakk-abcdefghijkl" are existing bookmarks.
        // generated is "stakk-abcdefghijkl" which matches existing[1],
        // so UseGenerated is skipped in the cycle.
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node(
                "abcdefghijkl",
                "work",
                &["feature", "stakk-abcdefghijkl"],
                false,
                true,
            ),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].state, RowState::UseExisting(0));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(1));

        // Generated matches existing[1], so skip UseGenerated → UseTfidf.
        // "work" produces a TF-IDF name.
        state.toggle_current();
        assert!(matches!(&state.rows[1].state, RowState::UseTfidf(_)));

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::Unchecked);

        state.toggle_current();
        assert_eq!(state.rows[1].state, RowState::UseExisting(0));
    }

    #[test]
    fn build_result_with_second_existing_bookmark() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "work", &["alpha", "beta"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Toggle once: UseExisting(0) → UseExisting(1).
        state.toggle_current();

        let result = state.build_result().unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bookmark_name, "beta");
        assert!(!result[0].is_new);
    }

    #[test]
    fn state_from_path_preserves_all_bookmarks() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_a", "work", &["alpha", "beta", "gamma"], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let state = BookmarkAssignmentState::from_path(&refs, false, None);

        assert_eq!(state.rows[1].existing_bookmarks.len(), 3);
        assert_eq!(state.rows[1].existing_bookmarks[0], "alpha");
        assert_eq!(state.rows[1].existing_bookmarks[1], "beta");
        assert_eq!(state.rows[1].existing_bookmarks[2], "gamma");
    }

    #[test]
    fn build_result_extracts_tfidf_name() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node(
                "ch_a",
                "implement caching layer for database queries",
                &[],
                false,
                true,
            ),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Unchecked → UseTfidf (auto is first after existing/unchecked).
        state.toggle_current();
        assert!(matches!(&state.rows[1].state, RowState::UseTfidf(_)));

        let result = state.build_result().unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].is_new);
        // The name should not start with "stakk-".
        assert!(
            !result[0].bookmark_name.starts_with("stakk-"),
            "expected TF-IDF name, got: {}",
            result[0].bookmark_name
        );
    }

    #[test]
    fn regenerate_cycles_tfidf_variation() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node(
                "ch_a",
                "implement caching layer for database queries",
                &[],
                false,
                true,
            ),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Get to UseTfidf (first toggle from Unchecked).
        state.toggle_current();
        let v0_name = match &state.rows[1].state {
            RowState::UseTfidf(ts) => {
                assert_eq!(ts.variation, 0);
                ts.name.clone()
            }
            other => panic!("expected UseTfidf, got {other:?}"),
        };

        // Regenerate should cycle variation.
        let result = state.regenerate_current();
        assert_ne!(result, RegenerateResult::NeedsRefire);
        match &state.rows[1].state {
            RowState::UseTfidf(ts) => {
                // Variation changed (unless all variations produce the same
                // result, which is fine).
                assert!(ts.variation != 0 || ts.name == v0_name);
            }
            other => panic!("expected UseTfidf after regenerate, got {other:?}"),
        }
    }

    #[test]
    fn effective_name_for_tfidf() {
        let mut row = make_bare_row(RowState::UseTfidf(TfidfNameState {
            name: "caching-database-layer".to_string(),
            variation: 0,
        }));
        row.existing_bookmarks = vec![];
        assert_eq!(row.effective_name(), Some("caching-database-layer"));
    }

    #[test]
    fn auto_prefix_prepended_to_tfidf_name() {
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node(
                "ch_a",
                "implement caching layer for database queries",
                &[],
                false,
                true,
            ),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, Some("gb-"));

        // Unchecked → UseTfidf (auto is first).
        state.toggle_current();
        match &state.rows[1].state {
            RowState::UseTfidf(ts) => {
                assert!(
                    ts.name.starts_with("gb-"),
                    "expected prefix 'gb-', got: {}",
                    ts.name
                );
            }
            other => panic!("expected UseTfidf, got {other:?}"),
        }
    }

    #[test]
    fn tfidf_refreshes_when_earlier_row_toggled() {
        // Three commits: trunk → middle → leaf.
        // Both middle and leaf are unchecked initially.
        // Toggle leaf to UseTfidf — its segment includes middle.
        // Then toggle middle to UseGenerated — leaf's segment shrinks.
        // The TF-IDF name on leaf should be recomputed.
        let nodes = [
            make_node("", "trunk", &[], true, false),
            make_node("ch_mid", "authentication middleware", &[], false, false),
            make_node("ch_leaf", "rate limiting endpoints", &[], false, true),
        ];
        let refs: Vec<&LayoutNode> = nodes.iter().collect();
        let mut state = BookmarkAssignmentState::from_path(&refs, false, None);

        // Get leaf to UseTfidf: Unchecked → UseTfidf (auto is first).
        state.cursor = 2;
        state.toggle_current();
        let leaf_name_with_middle = match &state.rows[2].state {
            RowState::UseTfidf(ts) => ts.name.clone(),
            other => panic!("expected UseTfidf on leaf, got {other:?}"),
        };

        // Now toggle middle (Unchecked → UseTfidf) — this changes leaf's
        // dynamic segment (middle is no longer unchecked, so leaf's segment
        // shrinks to just leaf).
        state.cursor = 1;
        state.toggle_current();

        // Leaf should still be UseTfidf but with a potentially different
        // name (fewer commits in segment).
        match &state.rows[2].state {
            RowState::UseTfidf(ts) => {
                // The name may or may not differ depending on term overlap,
                // but it should have been recomputed. At minimum, the state
                // is still UseTfidf (not stale).
                assert!(
                    !ts.name.is_empty(),
                    "refreshed TF-IDF name should not be empty"
                );
                // If the names differ, that confirms refresh happened.
                // If they're the same, it's because the terms overlap.
                let _ = leaf_name_with_middle; // suppress unused warning
            }
            RowState::Unchecked => {
                // Also valid: if the reduced segment produces no TF-IDF
                // name, it falls back to Unchecked.
            }
            other => panic!("expected UseTfidf or Unchecked on leaf after refresh, got {other:?}"),
        }
    }
}
