# CLAUDE.md — stakk

## Project Overview

**stakk** is a Rust CLI tool that bridges Jujutsu (`jj`) bookmarks to GitHub
stacked pull requests. It complements jj by turning local bookmark state into
coherent GitHub PRs with correct stacking order.

## Current Status

**v1.0.0** — All core features complete: stack detection, three-phase
submission, interactive TUI selection, stack comment templating, env var config,
and comprehensive error handling.

## Testing

- **Unit/integration tests**: `cargo nextest run --all-targets`.
- **Final pre-commit check**: `mise run ci` — run this after implementing plans
  and before committing.

## Development Principles

### 1. Never call git directly

All git operations go through `jj` commands (`jj git push`, `jj git remote
list`, `jj git fetch`, etc.). No `git` CLI calls, no `git2`, no `gix`.

### 2. Shell out to jj, don't link jj-lib

The `jj` CLI is the stable interface. Shell out and parse JSON/structured
output with serde. Always pass `--config 'ui.paginate=never'`.

### 3. Forge trait

All forge interaction goes through a `Forge` trait. The core submission logic
must never import GitHub-specific types directly.

### 4. Idempotent operations

Re-running any command must be safe. `submit` updates existing PRs rather than
creating duplicates. Stack comments are identified by embedded metadata.

### 5. Boring solutions over clever abstractions

Prefer simple, obvious code. Three similar lines are better than a premature
abstraction.

### 6. Test with fixtures

Capture real `jj` and GitHub API output as test fixtures. Tests should run
without a live jj repo or GitHub access.

### 7. No jj-stack compatibility

stakk uses its own comment metadata format (`STAKK_STACK` prefix), its own
serde field naming (snake_case), and its own comment footer.

## Architecture

```
src/
├── main.rs          # CLI entry point (clap)
├── auth.rs          # GitHub token resolution (gh CLI, env vars)
├── cli/             # clap subcommand definitions
├── jj/              # jj CLI interface — all VCS ops go here
├── forge/           # Forge trait + GitHub implementation (octocrab)
│   ├── mod.rs       # Forge trait, forge-agnostic types, ForgeError
│   ├── github.rs    # GitHubForge implementation
│   ├── comment.rs   # Stack comment formatting, parsing, and template context
│   └── default_comment.md.jinja  # Default minijinja template for stack comments
├── graph/           # Change graph construction (ChangeGraph, BookmarkSegment, BranchStack)
├── select/          # Interactive TUI selection (ratatui inline viewport)
│   ├── mod.rs       # Public API: resolve_bookmark_interactively(), SelectionResult
│   ├── app.rs       # App state machine, event loop, terminal init
│   ├── graph_layout.rs  # Convert ChangeGraph → 2D positioned nodes + edges
│   ├── graph_widget.rs  # Screen 1: tree graph widget (leaf selection)
│   ├── bookmark_widget.rs # Screen 2: bookmark toggle/assignment widget
│   ├── bookmark_gen.rs # Bookmark validation and external command execution
│   ├── tfidf.rs     # TF-IDF algorithm for auto-generated bookmark names
│   └── event.rs     # crossterm key event mapping to app actions
├── submit/          # Three-phase submission (analyze → plan → execute)
└── error.rs         # Error types (thiserror)
```

There is intentionally no `git/` module.

## Conventions

### Rust

- Edition 2024.
- Use `cargo nextest run` for testing, not `cargo test`.
- Prefer `cargo run --bin stakk` and `cargo build --bin stakk` over `-p stakk`.
- Find built binaries with:
  `cargo build --release --message-format json | jq -r 'select(.executable | . == null | not) | .executable'`
- **Never use `#[allow(...)]`**. Use `#[expect(..., reason = "...")]` instead,
  which requires a reason and warns when the expectation becomes unnecessary.

### Formatting

- `rustfmt.toml` uses nightly-only options (`format_strings`, `group_imports`,
  `imports_granularity`, `wrap_comments`, `doc_comment_code_block_width`).
- Run `mise run fmt:nightly` (or `cargo +nightly fmt --all`) for full
  formatting locally.
- **Always run `cargo +nightly fmt --all` before committing.**

### Version Control

- This repo uses `jj` (Jujutsu) for version control. Prefer `jj` over `git`.
- Before starting a new logical piece of work, verify a clean slate with
  `jj status`. If the current change is not empty, prompt the user or run
  `jj new`.
- Use `jj commit -m "message"` to finalize a change.
- Use `jj tug` to move the main bookmark forward to `@-` after committing.
- Push with `jj git push --bookmark main`.

### Error Handling

- Use `thiserror` + `miette::Diagnostic` for defining error enums everywhere.
- Concrete error types all the way up; `miette::Report` only at the `main()`
  boundary for rendering.
- No `anyhow` — every error is a concrete type with `Diagnostic` metadata.
- Use `#[diagnostic(help(...))]` for actionable advice on all variants.
  Use `#[diagnostic(code(stakk::...))]` for machine-readable error identifiers.

### jj Interface

- Always run jj with `--config 'ui.paginate=never'`.
- Use `--template` for structured/JSON output where available.
- Define serde structs for every piece of jj output we consume.
- Paginate large output (100 items at a time) to avoid memory issues.

## Selection TUI (select/)

Two-screen inline viewport TUI for choosing a branch path and assigning
bookmarks to commits.

### Screens

1. **GraphView** — Select a leaf node (branch tip) from the change graph.
   Navigate leaves with `←`/`→` (`h`/`l`), confirm with Enter.
   Help line: `◄►/hl navigate  Enter select  q/Esc quit`

2. **BookmarkAssignment** — Assign bookmark names to each commit in the
   selected path. Navigate rows with `↑`/`↓` (`j`/`k`). Rows are displayed
   in reverse order (leaf at top, trunk at bottom). Confirm with Enter,
   cancel back to GraphView with Esc/`q`.

### Bookmark Row State Cycle

Each non-trunk row cycles through state *types* via Space (forward) / `b`
(reverse). Each type is a single stop — Space never cycles within a type:

```
[x]use → [~]auto → [>]type → [+]new → [*]custom → [ ]skip
```

(`[*]custom` only present when `--bookmark-command` is configured.)

| Checkbox | State             | Color        | Description                                       |
|----------|-------------------|--------------|---------------------------------------------------|
| `[x]`   | UseExisting(idx)  | green, bold  | Use an existing bookmark; `r`/`R` cycles when >1  |
| `[~]`   | UseTfidf          | blue, bold   | TF-IDF name from commit description + files        |
| `[>]`   | UserInput         | lt-yellow    | Manual entry — press `i` to edit, validates live   |
| `[+]`   | UseGenerated      | yellow, bold | Auto `stakk-<change_id[:12]>`                      |
| `[*]`   | UseCustom         | cyan, bold   | External `--bookmark-command` with async spinner   |
| `[ ]`   | Unchecked         | dark gray    | Excluded from submission                           |

States are **skipped** when they would produce no name or a duplicate of
another state's name (e.g., UseTfidf skipped if it matches an existing
bookmark; UseGenerated skipped if it matches an existing one).

### Variation (`r`/`R`)

`r`/`R` cycles *within* the current state type:
- **UseExisting**: cycles through existing bookmarks (only when >1 exist).
- **UseTfidf**: cycles through up to 6 name variations.
- **UseCustom**: clears the cache and re-fires the external command.

### Dynamic TF-IDF Recomputation

The "dynamic segment" for a row = all included (toggled-on) commits from
trunk up to that row. Toggling a row recomputes UseTfidf names for all
subsequent rows in the stack.

### Edit Mode

Pressing `i` on a UserInput row enters edit mode (insert chars, Backspace
to delete, Esc/Enter to finish). Invalid names render in red. The help line
changes to: `Type name  Backspace delete  Esc/Enter done`.

### Context-Aware Help Line

The bookmark screen help line updates based on the currently selected row's
state, adding relevant keys (e.g., `r/R cycle` on UseExisting rows with >1
bookmark, `i edit` on UserInput rows, `r/R vary` on UseTfidf rows,
`r/R regenerate` on UseCustom rows).

### Validation on Confirm

Enter in BookmarkAssignment runs `build_result()` which validates git ref
rules, checks for duplicate names across included rows, and ensures no
bookmark is still loading. Errors display in the subtitle bar (red, bold).

### Async Bookmark Commands

UseCustom spawns a background task per row. Uses `BookmarkNameCache`
(keyed by commit IDs) with `Computing`/`Computed` entries. Computing
entries time out after 60s. The event loop polls at 80ms while commands
are in-flight to animate the spinner (10-frame animation).

## Patterns & Gotchas

- `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]` for fields used
  only in tests — satisfies both `--all-targets` clippy and `-D warnings`.
- jj JSON output uses NDJSON (one JSON object per line). Parse with `lines()`
  + per-line `serde_json::from_str`.
- `jj git remote list` outputs plain text, not JSON. Parse with string splitting.
- `trunk()` remote bookmarks include an internal `@git` entry — filter it out.
- Unsynced bookmarks produce duplicate entries in `jj bookmark list` —
  deduplicate by keeping only the first entry per name.
- Graph traversal uses `"trunk()"` as the revset base, not a branch name.
- octocrab treats PR comments as issue comments — use `issues().list_comments()`.
- octocrab `pulls().create()` borrows the handler — bind to a variable first.
- Commit-derived PR body is only set on creation, not on update — avoids
  overwriting manually-edited PR bodies. Body-mode stack placement updates
  only the fenced section.
- Stack comment metadata line (`<!--- STAKK_STACK: ... --->`) is always
  prepended programmatically — not part of the minijinja template.
  Warning/repo-URL preambles are added per-placement-mode: comment mode uses
  `with_comment_preamble()`, body mode adds `BODY_WARNING` inside the fence.
  `format_stack_comment` itself is placement-neutral (no warning lines).
  `STAKK_REPO_URL` is the single source of truth for the repo URL.
- `format_stack_comment` returns `Result` because user templates can fail.
- Body-mode fences (`STAKK_BODY_START`/`STAKK_BODY_END`) are HTML comments,
  invisible on GitHub. Migration between placement modes is automatic.
- ratatui inline viewport: `enable_raw_mode()` before, `disable_raw_mode()` after.
- Graph layout deduplicates shared segments by `commit_id` (not `change_id`).
- Auto-generated bookmark names: `stakk-<first 12 chars of change_id>`.

## Key Decisions

- **No jj-stack compatibility** — own `STAKK_STACK` prefix, snake_case serde.
- **No anyhow** — concrete error types with `Diagnostic` all the way up.
- **PR body on creation; body-mode updates fenced section only** — commit-derived
  body is set on PR creation and never overwritten. In `--stack-placement body`
  mode, only the fenced `STAKK_BODY_START`/`STAKK_BODY_END` section is updated.
- **`--dry-run` not in env vars** — one-off decision, surprising as a default.
- **Generic `Jj<R: JjRunner>`** — zero-cost dispatch, edition 2024 async traits.
- **Three-phase submission** — analyze (pure) → plan (queries forge) → execute.
- **ratatui over inquire** — visual graph rendering, bookmark assignment TUI.
- **minijinja for stack comments** — customizable templates, metadata outside template.
