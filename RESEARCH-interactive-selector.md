# Research: Interactive Selector Viewport & Graph Rendering

> **Date**: 2026-02-20
> **Context**: `stakk submit` (no args) shows an interactive bookmark picker,
> but the current implementation renders the entire graph at once. When there
> are many stacks/bookmarks, the output exceeds terminal height and scrolls
> past the top. This document summarizes research into three approaches for
> fixing this, plus analysis of how jj-stack solves it.

---

## Current Implementation (`src/select.rs`)

- `render_graph()` writes ALL lines to the terminal at once
- `clear_last_lines(line_count)` clears and re-renders on each keystroke
- No terminal height awareness (no `Term::size()` call)
- Linear display per stack (no branching/merging visualization)
- Already has: `○`/`│` characters, green focused, red ancestors, dim commits
- Uses `console` crate (already a direct dependency)

---

## How jj-stack Solves This

Source: `../jj-stack/src/cli/AnalyzeCommand.res` and
`AnalyzeCommandComponent.res`

### Architecture

jj-stack uses **Ink** (React for terminals) with ReScript. The rendering is
split into two phases:

1. **Pre-render** (`AnalyzeCommand.res:51-166`): Builds the full graph into
   an array of `outputRow` objects. Each row has a `chars` array (the column
   art) and an optional `changeId` (for selectable rows).

2. **Interactive display** (`AnalyzeCommandComponent.res`): Takes the
   pre-rendered rows and handles viewport windowing, keyboard navigation,
   and ancestor highlighting.

### Column-Based Graph Layout

jj-stack uses a column tracking system for branching visualization:

```
columns = []  // tracks which changeId occupies each column

for each change in topological order (leaf-to-root):
    find column for this change (or allocate new one)
    render "○" in that column, "│" in other active columns
    look up parent in adjacency list
    if parent already in a different column:
        draw merge line "─╯", remove current column
    else:
        replace current column with parent
```

Characters used: `" ○"` (node), `" │"` (continuation), `" ├"` (branch),
`"─╯"` (merge converging), `"─│"` (horizontal crossing vertical).

### Viewport Windowing

From `AnalyzeCommandComponent.res:39-92`:

```
terminal_height = stdout.rows
viewport_height = terminal_height - 3  // reserve header + footer + buffer

scroll_offset calculation:
  if total_items <= viewport_height: offset = 0 (everything fits)
  if focused < current_offset: offset = focused (scroll up)
  if focused >= current_offset + viewport_height: scroll down to show it
  else: keep current offset (no unnecessary jumping)

// Special case: when last item selected, always show trunk()
```

Only the visible slice `output[scroll_offset..scroll_offset+viewport_height]`
is rendered.

### Ancestor Highlighting

Walks the `bookmarkedChangeAdjacencyList` (child→parent map) starting from
the selected change, collecting all ancestors into a `Set<string>`. Any row
whose `changeId` is in the ancestor set renders in red.

### Navigation

Only rows with a `changeId` (bookmark rows) are selectable. Navigation
skips connector lines automatically using a pre-computed
`selectable_indices` array.

---

## Option A: Console + Viewport Windowing

Enhance the current `console`-based implementation with viewport logic.
No new dependencies.

### What Changes

**Pre-render phase** — new `build_graph_lines()` function produces a
`Vec<GraphLine>` separating data from rendering:

```rust
struct GraphLine {
    text: String,                   // unstyled text content
    bookmark_index: Option<usize>,  // Some(i) if selectable
    line_type: LineType,            // Bookmark, Commit, Connector, Separator, Trunk
}
```

**Viewport rendering** — new `render_viewport()` replaces `render_graph()`:

1. Get terminal height via `Term::size()` (returns `(cols, rows)`)
2. Reserve 2 lines for header/footer
3. Only render `lines[scroll_offset..scroll_offset+viewport_height]`
4. Show `▲ N more` / `▼ N more` indicators when content overflows
5. Apply styling (green/red/dim) at render time based on focused bookmark

**Scroll logic** — `calculate_scroll_offset()` as a pure function:

- Everything fits → offset 0
- Focused above viewport → scroll up
- Focused below viewport → scroll down
- Otherwise → keep current offset

**Navigation** — use pre-computed `selectable_indices: Vec<usize>` (line
indices where `bookmark_index.is_some()`). Arrow keys jump between
selectable lines.

### Optional Enhancement: DAG Graph Rendering

The `ChangeGraph` already has the data needed for jj-stack-style branching:

- `adjacency_list: HashMap<String, String>` — child→parent relationships
- `segments: HashMap<String, BookmarkSegment>` — per-change segment data
- `stack_leaves: HashSet<String>` — leaf nodes for topological sort start
- `stack_roots: HashSet<String>` — root nodes

A column-based layout algorithm (adapted from jj-stack) could replace the
current linear per-stack rendering. This would show stacks that share
ancestors merging visually, matching jj-stack's output.

### Console Crate Capabilities

Relevant APIs (all on `Term`, all take `&self`):

| Method | Purpose |
|--------|---------|
| `size() -> (u16, u16)` | Terminal dimensions (cols, rows) |
| `read_key() -> Result<Key>` | Read single keypress |
| `write_str(&str) -> Result<()>` | Write string (no `&mut self`) |
| `clear_last_lines(n) -> Result<()>` | Clear n preceding lines |
| `hide_cursor() / show_cursor()` | Cursor visibility |
| `is_term() -> bool` | TTY detection |

No alternate screen, no scroll regions. Viewport must be managed manually.

### Assessment

| Aspect | Rating |
|--------|--------|
| Dependency cost | None (console already a dep) |
| Implementation effort | Medium (~200-300 lines for viewport, ~200 more for DAG) |
| Viewport quality | Good (manual but follows proven jj-stack pattern) |
| Graph rendering | Excellent if DAG layout is added |
| Resize handling | Manual (re-query `Term::size()` on each render) |

---

## Option B: Ratatui Inline Viewport

Replace the custom renderer with ratatui's `Viewport::Inline(height)`.

### How It Works

Ratatui provides three viewport modes:

- **Fullscreen** — alternate screen buffer, replaces terminal view entirely
- **Inline(height)** — allocates a fixed rectangular area inline with normal
  terminal output, preserving scrollback history
- **Fixed** — render to a specific terminal area

`Viewport::Inline` is the best fit: it keeps the CLI feel (no alternate
screen takeover) while providing a proper rendering surface.

```rust
let terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(height),
});
```

### List Widget

Ratatui has a built-in `List` + `ListState` that handles scrolling
automatically:

- `select_next()` / `select_previous()` — navigate items
- Automatic viewport scrolling to keep selection visible
- No manual scroll offset management needed
- Supports custom styling per item

### Backend

Ratatui uses **crossterm** as its default backend. Crossterm handles raw
mode, key events, and terminal manipulation.

**Conflict note**: crossterm and `console` both manipulate terminal state.
Using both simultaneously could cause issues (competing raw mode, event
queues). If ratatui is adopted, the `console` dependency might need to be
removed or isolated.

### Dependency Weight

Ratatui and crossterm are **not** currently in stakk's dependency tree.
Adding them brings:

- `ratatui` (modular since 0.30+, backends are feature-gated)
- `crossterm` (default backend)
- Various transitive deps

### Known Issues

- Inline viewport resizing has edge cases (ratatui issues #984, #2086)
- Must handle `Event::Resize` and redraw
- Some backends have rendering artifacts with inline viewports

### Assessment

| Aspect | Rating |
|--------|--------|
| Dependency cost | High (ratatui + crossterm, new dep tree) |
| Implementation effort | Medium (rewrite select.rs, ~250 lines) |
| Viewport quality | Excellent (built-in scrolling, resize events) |
| Graph rendering | Custom widget needed (same effort as Option A) |
| Resize handling | Built-in event system |

---

## Option C: Inquire Two-Step Selection

Replace the custom graph renderer with `inquire::Select` prompts.

### How It Works

Two sequential prompts:

1. **Stack selection** — compact single-line representation of each stack:
   ```
   ? Which stack to submit?
   > feat-a -> feat-b -> feat-c (3 bookmarks)
     standalone-fix (1 bookmark)
     user-api -> user-model (2 bookmarks)
   ```

2. **Bookmark selection** (if stack has multiple bookmarks):
   ```
   ? Submit up to which bookmark in this stack?
   > feat-c (leaf)
     feat-b
     feat-a (closest to trunk)
   ```

### Features for Free

- **Auto-viewport**: Paginated with configurable page size (default 7)
- **Type-to-filter**: Fuzzy search enabled by default (SkimV2 algorithm)
- **Circular navigation**: Wraps around at list boundaries
- **Styling**: `RenderConfig` for colors and tokens (global, not per-item)

### Dependency

```toml
inquire = { version = "0.7", default-features = false, features = ["console"] }
```

The `console` backend feature reuses our existing `console` dependency,
avoiding crossterm. Net new transitive dep: `fuzzy_matcher` only.

### Limitations

- **Single-line items only** — no multi-line graph art per item
- **No graph visualization** — loses the `○`/`│` tree structure
- **Two-step friction** — extra prompt for the common single-stack case
  (could auto-skip step 1 when there's only one stack)
- **Per-item coloring not supported** — `RenderConfig` applies globally

### Assessment

| Aspect | Rating |
|--------|--------|
| Dependency cost | Low (inquire with console backend, ~1 new transitive dep) |
| Implementation effort | Low (~50-80 lines total) |
| Viewport quality | Excellent (built-in pagination) |
| Graph rendering | None (text-only compact representation) |
| Resize handling | Handled by inquire |

---

## Comparison Summary

| | Option A: Console | Option B: Ratatui | Option C: Inquire |
|---|---|---|---|
| **New deps** | None | ratatui + crossterm | inquire (console backend) |
| **Viewport** | Manual (proven pattern) | Built-in | Built-in |
| **Graph art** | Yes (current + DAG possible) | Yes (custom widget) | No |
| **jj-stack parity** | Closest match | Close (different rendering) | Different UX |
| **Code complexity** | ~400-500 lines | ~250 lines | ~50-80 lines |
| **Type-to-filter** | No | No (without extra work) | Yes (free) |
| **Risk** | Low (no new deps) | Medium (dep conflicts) | Low |

---

## Recommendation

**Option A** for the graph rendering and viewport — it matches jj-stack's
proven approach, adds no dependencies, and gives full control. The
implementation splits naturally into two parts:

1. **Viewport windowing** (immediate fix) — add `Term::size()`, scroll
   offset tracking, visible-window rendering. Fixes the scrolling bug.
2. **DAG graph layout** (follow-up) — column-based rendering adapted from
   jj-stack. Visual enhancement showing true branching structure.

Option C (inquire) remains a viable fallback or complement — it could be
used for a `--simple` flag or as the non-TTY fallback's error message
suggestion.
