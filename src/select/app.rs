//! App state machine and event loop for the TUI selection.
//!
//! Two screens: `GraphView` (pick a branch) → `BookmarkAssignment` (toggle
//! bookmarks). Uses ratatui's inline viewport (not fullscreen).

use std::io;

use crossterm::event;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::SelectionResult;
use super::bookmark_widget::BookmarkAssignmentState;
use super::bookmark_widget::BookmarkWidget;
use super::bookmark_widget::bookmark_help_line;
use super::event::Action;
use super::event::map_event;
use super::graph_layout::GraphLayout;
use super::graph_layout::build_layout;
use super::graph_layout::path_to_leaf;
use super::graph_widget::GraphViewState;
use super::graph_widget::GraphWidget;
use super::graph_widget::display_line_count;
use super::graph_widget::graph_help_line;
use crate::error::StakkError::Interrupted;
use crate::error::StakkError::{self};
use crate::graph::types::ChangeGraph;

/// Which screen is currently active.
enum Screen {
    /// Viewing the graph and selecting a branch.
    GraphView,
    /// Assigning bookmarks on the selected path.
    BookmarkAssignment,
}

/// Run the TUI selection flow.
///
/// Returns `Ok(Some(result))` on successful selection, `Ok(None)` if the user
/// cancels from the graph view.
pub fn run_tui(graph: &ChangeGraph) -> Result<Option<SelectionResult>, StakkError> {
    let layout = build_layout(graph);

    if layout.leaf_nodes().is_empty() {
        eprintln!("No selectable branches found.");
        return Ok(None);
    }

    let mut graph_state = GraphViewState::new();
    let mut bookmark_state: Option<BookmarkAssignmentState> = None;
    let mut screen = Screen::GraphView;

    // Calculate viewport height: content rows + title + subtitle + help.
    let (_, term_height) = crossterm::terminal::size()?;
    let content_height = display_line_count(layout.total_rows) + 3;
    let viewport_height = u16::try_from(content_height.min(30).min(usize::from(term_height) - 2))
        .expect("viewport height fits in u16");

    // Set up inline viewport.
    crossterm::terminal::enable_raw_mode()?;

    let backend = CrosstermBackend::new(io::stderr());
    let options = ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Inline(viewport_height),
    };
    let mut terminal = Terminal::with_options(backend, options)?;

    let result = run_event_loop(
        &mut terminal,
        &layout,
        &mut graph_state,
        &mut bookmark_state,
        &mut screen,
    );

    // Clear the inline viewport so subsequent output doesn't interleave with TUI
    // remnants. Moves cursor to viewport top, erases to end of screen, shows
    // cursor. Use let _ to avoid masking errors from the event loop result.
    let _ = terminal.clear();
    let _ = terminal.show_cursor();

    // Restore terminal.
    crossterm::terminal::disable_raw_mode()?;
    // Blank line between TUI area and subsequent output.
    eprintln!();

    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    layout: &GraphLayout,
    graph_state: &mut GraphViewState,
    bookmark_state: &mut Option<BookmarkAssignmentState>,
    screen: &mut Screen,
) -> Result<Option<SelectionResult>, StakkError> {
    loop {
        // Draw.
        terminal.draw(|frame| {
            let area = frame.area();

            // Split into title + subtitle + content + help.
            let chunks = Layout::vertical([
                Constraint::Length(1), // Title (bold)
                Constraint::Length(1), // Subtitle (dim)
                Constraint::Min(1),    // Content
                Constraint::Length(1), // Help
            ])
            .split(area);

            match screen {
                Screen::GraphView => {
                    render_graph_screen(
                        frame,
                        chunks[0],
                        chunks[1],
                        chunks[2],
                        chunks[3],
                        layout,
                        graph_state,
                    );
                }
                Screen::BookmarkAssignment => {
                    if let Some(bm_state) = bookmark_state.as_ref() {
                        render_bookmark_screen(
                            frame, chunks[0], chunks[1], chunks[2], chunks[3], bm_state,
                        );
                    }
                }
            }
        })?;

        // Handle input.
        let ev = event::read()?;
        let action = map_event(&ev);

        match screen {
            Screen::GraphView => match action {
                Action::Left => {
                    // ←/h: move selection to a lower column (lower index).
                    let leaves = layout.leaf_nodes();
                    if graph_state.selected_leaf > 0 {
                        graph_state.selected_leaf -= 1;
                    } else {
                        graph_state.selected_leaf = leaves.len().saturating_sub(1);
                    }
                }
                Action::Right => {
                    // →/l: move selection to a higher column (higher index).
                    let leaves = layout.leaf_nodes();
                    if graph_state.selected_leaf < leaves.len().saturating_sub(1) {
                        graph_state.selected_leaf += 1;
                    } else {
                        graph_state.selected_leaf = 0;
                    }
                }
                Action::Select => {
                    let leaves = layout.leaf_nodes();
                    if let Some(leaf) = leaves.get(graph_state.selected_leaf) {
                        let path = path_to_leaf(layout, leaf.row, leaf.col);
                        *bookmark_state = Some(BookmarkAssignmentState::from_path(&path));
                        *screen = Screen::BookmarkAssignment;
                    }
                }
                Action::Cancel => {
                    return Ok(None);
                }
                Action::Quit => {
                    return Err(Interrupted);
                }
                Action::Up | Action::Down | Action::Toggle | Action::None => {}
            },
            Screen::BookmarkAssignment => match action {
                Action::Up => {
                    if let Some(state) = bookmark_state {
                        state.cursor_up();
                    }
                }
                Action::Down => {
                    if let Some(state) = bookmark_state {
                        state.cursor_down();
                    }
                }
                Action::Toggle => {
                    if let Some(state) = bookmark_state {
                        state.toggle_current();
                    }
                }
                Action::Select => {
                    if let Some(state) = bookmark_state.as_ref() {
                        let assignments = state.build_result();
                        if assignments.is_empty() {
                            // Nothing selected — don't proceed.
                            continue;
                        }
                        return Ok(Some(SelectionResult { assignments }));
                    }
                }
                Action::Cancel => {
                    *screen = Screen::GraphView;
                    *bookmark_state = None;
                }
                Action::Quit => {
                    return Err(Interrupted);
                }
                Action::Left | Action::Right | Action::None => {}
            },
        }
    }
}

fn render_graph_screen(
    frame: &mut ratatui::Frame,
    title_area: Rect,
    subtitle_area: Rect,
    content_area: Rect,
    help_area: Rect,
    layout: &GraphLayout,
    state: &GraphViewState,
) {
    let title = Line::from(vec![Span::styled(
        " Select branch stack",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(title, title_area);

    let subtitle = Line::from(vec![Span::styled(
        " The highlighted branch will be submitted as a stack of pull requests.",
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )]);
    frame.render_widget(subtitle, subtitle_area);

    let widget = GraphWidget::new(layout, state);
    widget.render(content_area, frame.buffer_mut());

    frame.render_widget(graph_help_line(), help_area);
}

fn render_bookmark_screen(
    frame: &mut ratatui::Frame,
    title_area: Rect,
    subtitle_area: Rect,
    content_area: Rect,
    help_area: Rect,
    state: &BookmarkAssignmentState,
) {
    let title = Line::from(vec![Span::styled(
        " Assign bookmarks to commits",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(title, title_area);

    let subtitle = Line::from(vec![Span::styled(
        " Checked commits ([x]/[+]) will be pushed and have PRs created or updated.",
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )]);
    frame.render_widget(subtitle, subtitle_area);

    let widget = BookmarkWidget::new(state);
    widget.render(content_area, frame.buffer_mut());

    frame.render_widget(bookmark_help_line(), help_area);
}
