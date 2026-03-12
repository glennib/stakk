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
│   ├── graph_widget.rs  # Screen 1: tree graph widget
│   ├── bookmark_widget.rs # Screen 2: bookmark toggle/assignment widget
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
- PR body is only set on creation, not on update — avoids overwriting
  manually-edited PR bodies.
- Stack comment metadata line (`<!--- STAKK_STACK: ... --->`) is always
  prepended programmatically — not part of the minijinja template.
- `format_stack_comment` returns `Result` because user templates can fail.
- ratatui inline viewport: `enable_raw_mode()` before, `disable_raw_mode()` after.
- Graph layout deduplicates shared segments by `commit_id` (not `change_id`).
- Auto-generated bookmark names: `stakk-<first 12 chars of change_id>`.

## Key Decisions

- **No jj-stack compatibility** — own `STAKK_STACK` prefix, snake_case serde.
- **No anyhow** — concrete error types with `Diagnostic` all the way up.
- **PR body only on creation** — never overwrites manually-edited PR bodies.
- **`--dry-run` not in env vars** — one-off decision, surprising as a default.
- **Generic `Jj<R: JjRunner>`** — zero-cost dispatch, edition 2024 async traits.
- **Three-phase submission** — analyze (pure) → plan (queries forge) → execute.
- **ratatui over inquire** — visual graph rendering, bookmark assignment TUI.
- **minijinja for stack comments** — customizable templates, metadata outside template.
