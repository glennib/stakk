//! App state machine and event loop for the TUI selection.
//!
//! Two screens: `GraphView` (pick a branch) → `BookmarkAssignment` (toggle
//! bookmarks). Uses ratatui's inline viewport (not fullscreen).

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

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
use super::bookmark_gen;
use super::bookmark_gen::BookmarkGenError;
use super::bookmark_gen::BookmarkNameCache;
use super::bookmark_gen::CacheEntry;
use super::bookmark_widget::BookmarkAssignmentState;
use super::bookmark_widget::BookmarkWidget;
use super::bookmark_widget::CustomNameState;
use super::bookmark_widget::InputMode;
use super::bookmark_widget::RowState;
use super::bookmark_widget::SelectionError;
use super::bookmark_widget::VaryResult;
use super::bookmark_widget::bookmark_help_line;
use super::event::Action;
use super::event::EditAction;
use super::event::map_event;
use super::event::map_event_editing;
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

/// A pending bookmark command: row index + oneshot receiver for the result.
struct PendingCommand {
    row_idx: usize,
    rx: tokio::sync::oneshot::Receiver<Result<String, BookmarkGenError>>,
}

/// Run the TUI selection flow.
///
/// Returns `Ok(Some(result))` on successful selection, `Ok(None)` if the user
/// cancels from the graph view.
pub fn run_tui(
    graph: &ChangeGraph,
    bookmark_command: Option<&str>,
    auto_prefix: Option<&str>,
) -> Result<Option<SelectionResult>, StakkError> {
    let layout = build_layout(graph);
    let has_bookmark_command = bookmark_command.is_some();
    let bookmark_cache = Arc::new(Mutex::new(BookmarkNameCache::new()));

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
        has_bookmark_command,
        bookmark_command,
        auto_prefix,
        &bookmark_cache,
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

#[expect(
    clippy::too_many_arguments,
    reason = "TUI event loop needs all context threaded through"
)]
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    layout: &GraphLayout,
    graph_state: &mut GraphViewState,
    bookmark_state: &mut Option<BookmarkAssignmentState>,
    screen: &mut Screen,
    has_bookmark_command: bool,
    bookmark_command: Option<&str>,
    auto_prefix: Option<&str>,
    bookmark_cache: &Arc<Mutex<BookmarkNameCache>>,
) -> Result<Option<SelectionResult>, StakkError> {
    let mut pending: Vec<PendingCommand> = Vec::new();
    let mut spinner_tick: usize = 0;
    let mut error_message: Option<String> = None;

    loop {
        // Check for completed background commands before drawing.
        if let Some(msg) = drain_completed(&mut pending, bookmark_state) {
            error_message = Some(msg);
        }

        // Resolve Loading rows whose cache key now has a Computed entry
        // (e.g. from an orphaned task or a task spawned for a different row
        // with the same segment key).
        resolve_cached_names(bookmark_state, bookmark_cache);

        let has_loading = bookmark_state.as_ref().is_some_and(|s| {
            s.rows
                .iter()
                .any(|r| matches!(r.state, RowState::UseCustom(CustomNameState::Loading)))
        });
        let has_pending = !pending.is_empty() || has_loading;

        // Update spinner state for loading indicators.
        if has_pending {
            spinner_tick = spinner_tick.wrapping_add(1);
        }

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
                            frame,
                            chunks[0],
                            chunks[1],
                            chunks[2],
                            chunks[3],
                            bm_state,
                            spinner_tick,
                            bookmark_command,
                            error_message.as_deref(),
                        );
                    }
                }
            }
        })?;

        // Poll for input with a short timeout so we can re-render spinner
        // frames while commands are in-flight.
        let poll_timeout = if has_pending {
            Duration::from_millis(80)
        } else {
            Duration::from_secs(60)
        };

        if !event::poll(poll_timeout)? {
            continue; // No input — re-render (updates spinner).
        }

        let ev = event::read()?;

        match screen {
            Screen::GraphView => {
                let action = map_event(&ev);
                match action {
                    Action::Left => {
                        let leaves = layout.leaf_nodes();
                        if graph_state.selected_leaf > 0 {
                            graph_state.selected_leaf -= 1;
                        } else {
                            graph_state.selected_leaf = leaves.len().saturating_sub(1);
                        }
                    }
                    Action::Right => {
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
                            *bookmark_state = Some(BookmarkAssignmentState::from_path(
                                &path,
                                has_bookmark_command,
                                auto_prefix,
                            ));
                            *screen = Screen::BookmarkAssignment;
                        }
                    }
                    Action::Cancel => {
                        return Ok(None);
                    }
                    Action::Quit => {
                        return Err(Interrupted);
                    }
                    Action::Up
                    | Action::Down
                    | Action::Toggle
                    | Action::ReverseToggle
                    | Action::EnterEdit
                    | Action::Vary
                    | Action::ReverseVary
                    | Action::None => {}
                }
            }
            Screen::BookmarkAssignment => {
                // Edit-mode dispatch: consume the event and continue without
                // falling through to normal action handling.
                let is_editing = bookmark_state
                    .as_ref()
                    .is_some_and(|s| s.input_mode == InputMode::Editing);

                if is_editing {
                    if let Some(edit_action) = map_event_editing(&ev) {
                        match edit_action {
                            EditAction::InsertChar(c) => {
                                if let Some(state) = bookmark_state.as_mut() {
                                    state.insert_char(c);
                                }
                            }
                            EditAction::Backspace => {
                                if let Some(state) = bookmark_state.as_mut() {
                                    state.delete_char();
                                }
                            }
                            EditAction::ExitEdit => {
                                if let Some(state) = bookmark_state.as_mut() {
                                    state.exit_edit_mode();
                                }
                            }
                            EditAction::Quit => {
                                return Err(Interrupted);
                            }
                        }
                    }
                    continue;
                }

                // Normal-mode dispatch.
                let action = map_event(&ev);
                match action {
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
                    Action::Toggle | Action::ReverseToggle => {
                        error_message = None;
                        if let Some(state) = bookmark_state {
                            if action == Action::ReverseToggle {
                                state.toggle_current_reverse();
                            } else {
                                state.toggle_current();
                            }

                            if let Some(cmd) = bookmark_command {
                                fire_pending_commands(state, cmd, bookmark_cache, &mut pending);
                            }
                        }
                    }
                    Action::Select => {
                        if let Some(state) = bookmark_state.as_ref() {
                            match state.build_result() {
                                Ok(assignments) if assignments.is_empty() => {}
                                Ok(assignments) => {
                                    return Ok(Some(SelectionResult { assignments }));
                                }
                                Err(SelectionError::DuplicateName(name)) => {
                                    error_message =
                                        Some(format!("Duplicate bookmark name: {name}"));
                                }
                                Err(SelectionError::StillLoading) => {
                                    error_message =
                                        Some("A bookmark name is still loading…".to_string());
                                }
                                Err(SelectionError::InvalidName(msg)) => {
                                    error_message = Some(format!("Invalid bookmark name: {msg}"));
                                }
                            }
                        }
                    }
                    Action::EnterEdit => {
                        if let Some(state) = bookmark_state.as_mut() {
                            state.enter_edit_mode();
                        }
                    }
                    Action::Vary | Action::ReverseVary => {
                        error_message = None;
                        if let Some(state) = bookmark_state {
                            let result = if action == Action::ReverseVary {
                                state.vary_current_reverse()
                            } else {
                                state.vary_current()
                            };
                            match result {
                                VaryResult::NeedsRefire => {
                                    if let Some(cmd) = bookmark_command {
                                        fire_pending_commands(
                                            state,
                                            cmd,
                                            bookmark_cache,
                                            &mut pending,
                                        );
                                    }
                                }
                                VaryResult::TfidfNoVariation => {
                                    error_message =
                                        Some("No other auto-name variations available".to_string());
                                }
                                VaryResult::ExistingCycled
                                | VaryResult::TfidfCycled
                                | VaryResult::Noop => {}
                            }
                        }
                    }
                    Action::Cancel => {
                        pending.clear();
                        *screen = Screen::GraphView;
                        *bookmark_state = None;
                    }
                    Action::Quit => {
                        return Err(Interrupted);
                    }
                    Action::Left | Action::Right | Action::None => {}
                }
            }
        }
    }
}

/// Spawn background tasks for any `UseCustom(Loading)` rows. Also detects
/// invalidated `UseCustom` rows whose dynamic segment changed. Deduplicates
/// against in-flight `CacheEntry::Computing` entries and applies synchronous
/// `CacheEntry::Computed` hits without spawning.
fn fire_pending_commands(
    state: &mut BookmarkAssignmentState,
    command: &str,
    cache: &Arc<Mutex<BookmarkNameCache>>,
    pending: &mut Vec<PendingCommand>,
) {
    let mut cache_guard = cache.lock().expect("cache mutex poisoned");

    let needs_fire: Vec<usize> = state
        .rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| match &row.state {
            RowState::UseCustom(CustomNameState::Loading) => {
                let segment = bookmark_gen::dynamic_segment_commits(&state.rows, i);
                let key = bookmark_gen::cache_key(&segment);
                match cache_guard.get(&key) {
                    // Already in-flight and not expired — skip.
                    Some(CacheEntry::Computing { .. })
                        if !cache_guard.get(&key).unwrap().is_expired() =>
                    {
                        None
                    }
                    // Expired Computing — remove so we can retry.
                    Some(CacheEntry::Computing { .. }) => {
                        cache_guard.remove(&key);
                        Some(i)
                    }
                    // Computed (apply synchronously below) or absent (needs
                    // spawn).
                    Some(CacheEntry::Computed(_)) | None => Some(i),
                }
            }
            RowState::UseCustom(CustomNameState::Ready(_)) => {
                // Check if the dynamic segment changed.
                let segment = bookmark_gen::dynamic_segment_commits(&state.rows, i);
                let key = bookmark_gen::cache_key(&segment);
                if let Some(CacheEntry::Computed(cached_name)) = cache_guard.get(&key)
                    && row.custom_name.as_ref() == Some(cached_name)
                {
                    return None;
                }
                Some(i)
            }
            _ => None,
        })
        .collect();

    if needs_fire.is_empty() {
        return;
    }

    let handle = tokio::runtime::Handle::current();

    for idx in &needs_fire {
        let idx = *idx;

        // Remove any existing pending command for this row.
        pending.retain(|p| p.row_idx != idx);

        // Check cache (synchronous hit — e.g. from an orphaned task or
        // Computed entry detected above).
        let segment = bookmark_gen::dynamic_segment_commits(&state.rows, idx);
        let key = bookmark_gen::cache_key(&segment);
        match cache_guard.get(&key) {
            Some(CacheEntry::Computed(cached_name)) => {
                let name = cached_name.clone();
                state.rows[idx].custom_name = Some(name.clone());
                state.rows[idx].state = RowState::UseCustom(CustomNameState::Ready(name));
                continue;
            }
            Some(CacheEntry::Computing { .. }) if !cache_guard.get(&key).unwrap().is_expired() => {
                // Already in-flight — ensure row shows loading.
                state.rows[idx].state = RowState::UseCustom(CustomNameState::Loading);
                continue;
            }
            _ => {
                // No entry or expired — proceed to spawn.
            }
        }

        // Mark as Computing before spawning.
        cache_guard.insert(
            key.clone(),
            CacheEntry::Computing {
                since: std::time::Instant::now(),
            },
        );

        // Build the input before spawning (we need to read from state).
        let input = bookmark_gen::build_segment_input(&segment);
        let json = serde_json::to_string(&input).expect("SegmentInput is always serializable");
        let cmd = command.to_string();
        let task_cache = Arc::clone(cache);

        let (tx, rx) = tokio::sync::oneshot::channel();

        handle.spawn(async move {
            let result = bookmark_gen::run_command(&cmd, &json).await;

            if let Ok(mut guard) = task_cache.lock() {
                match &result {
                    Ok(name) if bookmark_gen::validate_bookmark_name(name).is_ok() => {
                        guard.insert(key, CacheEntry::Computed(name.clone()));
                    }
                    _ => {
                        // Remove the Computing entry so the key can be retried.
                        guard.remove(&key);
                    }
                }
            }

            let _ = tx.send(result);
        });

        // Set loading state.
        state.rows[idx].state = RowState::UseCustom(CustomNameState::Loading);

        pending.push(PendingCommand { row_idx: idx, rx });
    }
}

/// Drain completed background commands and update row state.
///
/// The spawned tasks already validate and cache on success, so this function
/// only updates the TUI row state from the result. Guards against overwriting
/// state if the user toggled away from `UseCustom(Loading)` while the task was
/// running.
fn drain_completed(
    pending: &mut Vec<PendingCommand>,
    bookmark_state: &mut Option<BookmarkAssignmentState>,
) -> Option<String> {
    let Some(state) = bookmark_state.as_mut() else {
        pending.clear();
        return None;
    };
    let mut first_error: Option<String> = None;

    let mut completed = Vec::new();
    for (i, cmd) in pending.iter_mut().enumerate() {
        match cmd.rx.try_recv() {
            Ok(result) => completed.push((i, cmd.row_idx, result)),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {} // Still running.
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                // Sender dropped without sending — treat as error.
                completed.push((
                    i,
                    cmd.row_idx,
                    Err(BookmarkGenError::EmptyOutput {
                        command: "(dropped)".to_string(),
                    }),
                ));
            }
        }
    }

    // Process in reverse index order so removal doesn't shift indices.
    completed.sort_by(|a, b| b.0.cmp(&a.0));
    for (pending_idx, row_idx, result) in completed {
        pending.remove(pending_idx);

        if row_idx >= state.rows.len() {
            continue;
        }

        match result {
            Ok(name) => {
                if bookmark_gen::validate_bookmark_name(&name).is_ok() {
                    state.rows[row_idx].custom_name = Some(name.clone());
                    if matches!(
                        state.rows[row_idx].state,
                        RowState::UseCustom(CustomNameState::Loading)
                    ) {
                        state.rows[row_idx].state =
                            RowState::UseCustom(CustomNameState::Ready(name));
                    }
                } else {
                    let err = bookmark_gen::validate_bookmark_name(&name).unwrap_err();
                    if first_error.is_none() {
                        first_error = Some(format!("Bookmark command: {err}"));
                    }
                    if matches!(
                        state.rows[row_idx].state,
                        RowState::UseCustom(CustomNameState::Loading)
                    ) {
                        state.rows[row_idx].custom_name = None;
                        state.rows[row_idx].state = RowState::Unchecked;
                    }
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("Bookmark command: {e}"));
                }
                if matches!(
                    state.rows[row_idx].state,
                    RowState::UseCustom(CustomNameState::Loading)
                ) {
                    state.rows[row_idx].custom_name = None;
                    state.rows[row_idx].state = RowState::Unchecked;
                }
            }
        }
    }

    first_error
}

/// Resolve `UseCustom(Loading)` rows whose cache key now has a `Computed`
/// entry. This catches rows that were set to `Loading` because a `Computing`
/// entry was already in-flight for their segment key — those rows have no
/// `PendingCommand`, so `drain_completed` can't resolve them.
fn resolve_cached_names(
    bookmark_state: &mut Option<BookmarkAssignmentState>,
    cache: &Arc<Mutex<BookmarkNameCache>>,
) {
    let Some(state) = bookmark_state.as_mut() else {
        return;
    };

    let cache_guard = cache.lock().expect("cache mutex poisoned");

    // Collect (row_index, resolved_name) pairs first to avoid borrow conflict.
    let resolved: Vec<(usize, String)> = state
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row.state, RowState::UseCustom(CustomNameState::Loading)))
        .filter_map(|(i, _)| {
            let segment = bookmark_gen::dynamic_segment_commits(&state.rows, i);
            let key = bookmark_gen::cache_key(&segment);
            if let Some(CacheEntry::Computed(name)) = cache_guard.get(&key) {
                Some((i, name.clone()))
            } else {
                None
            }
        })
        .collect();

    for (i, name) in resolved {
        state.rows[i].custom_name = Some(name.clone());
        state.rows[i].state = RowState::UseCustom(CustomNameState::Ready(name));
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

#[expect(
    clippy::too_many_arguments,
    reason = "render function needs all layout areas plus state"
)]
fn render_bookmark_screen(
    frame: &mut ratatui::Frame,
    title_area: Rect,
    subtitle_area: Rect,
    content_area: Rect,
    help_area: Rect,
    state: &BookmarkAssignmentState,
    spinner_tick: usize,
    bookmark_command: Option<&str>,
    error_message: Option<&str>,
) {
    let title = Line::from(vec![Span::styled(
        " Assign bookmarks to commits",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(title, title_area);

    let subtitle = if let Some(err) = error_message {
        Line::from(vec![Span::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )])
    } else {
        Line::from(vec![Span::styled(
            " Checked commits ([x]/[+]/[~]/[*]) will be pushed and have PRs created or updated.",
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
        )])
    };
    frame.render_widget(subtitle, subtitle_area);

    let editing = state.is_editing();
    let editing_row = if editing { Some(state.cursor) } else { None };
    let widget = BookmarkWidget::new(state, spinner_tick, bookmark_command, editing_row);
    widget.render(content_area, frame.buffer_mut());

    frame.render_widget(
        bookmark_help_line(
            bookmark_command.is_some(),
            editing,
            state.rows.get(state.cursor).map(|r| &r.state),
            state
                .rows
                .get(state.cursor)
                .map_or(0, |r| r.existing_bookmarks.len()),
        ),
        help_area,
    );
}
