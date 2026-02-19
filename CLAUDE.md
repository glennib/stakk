# CLAUDE.md — jack

> **IMPORTANT**: Agents MUST update this file as part of every planning step.
> This includes: recording progress on milestones, documenting new patterns or
> conventions discovered during development, updating principles when they
> evolve, noting architectural decisions made during implementation, and keeping
> the current status accurate. Treat this file as the living source of truth for
> how we build jack.

## Project Overview

**jack** is a Rust rewrite of [jj-stack](https://github.com/keanemind/jj-stack)
— a CLI tool that bridges Jujutsu (`jj`) bookmarks to GitHub stacked pull
requests. It is not a jj wrapper; it complements jj by turning local bookmark
state into coherent GitHub PRs with correct stacking order.

See [ANALYSIS.md](ANALYSIS.md) for full research and [ROADMAP.md](ROADMAP.md)
for milestones.

## Current Status

- **Milestone 0 (Project Skeleton)**: Complete.
- **Milestone 1 (jj Interface Layer)**: Complete — `Jj<R>` struct with
  `JjRunner` trait, serde types for jj JSON output, GitHub URL parsing,
  6 public methods, 32 unit/integration tests.

## Development Principles

### 1. Never call git directly

All git operations go through `jj` commands (`jj git push`, `jj git remote
list`, `jj git fetch`, etc.). No `git` CLI calls, no `git2`, no `gix`. This
single rule makes jj workspaces and non-colocated repos work automatically with
zero special-case handling.

### 2. Shell out to jj, don't link jj-lib

The `jj` CLI is the stable interface. `jj-lib` is pre-1.0 and subject to
breaking changes. We shell out to `jj` and parse JSON/structured output with
serde. Always pass `--config 'ui.paginate=never'` to avoid pager issues.

### 3. Forge trait from day one

GitHub is the first implementation, but all forge interaction goes through a
`Forge` trait. The core submission logic must never import GitHub-specific types
directly — only the trait.

### 4. Idempotent operations

Re-running any command must be safe. `submit` updates existing PRs rather than
creating duplicates. Stack comments are identified by embedded metadata so they
can be updated in place.

### 5. Boring solutions over clever abstractions

Prefer simple, obvious code. Three similar lines are better than a premature
abstraction. Don't design for hypothetical future requirements. Each abstraction
must justify itself with a concrete current need.

### 6. Minimum viable at each milestone

Each milestone in [ROADMAP.md](ROADMAP.md) produces something testable and
usable. Don't gold-plate early milestones with features from later ones.

### 7. Test with fixtures

Capture real `jj` and GitHub API output as test fixtures. Tests should run
without a live jj repo or GitHub access. This makes CI fast and deterministic.

## Architecture

```
src/
├── main.rs          # CLI entry point (clap)
├── cli/             # clap subcommand definitions
├── jj/              # jj CLI interface — all VCS ops go here
├── forge/           # Forge trait + GitHub implementation (octocrab)
├── graph/           # Change graph construction (ChangeGraph, BookmarkSegment, BranchStack)
├── submit/          # Three-phase submission (analyze → plan → execute)
└── error.rs         # Error types (thiserror)
```

There is intentionally no `git/` module.

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` (v4, derive) | CLI framework |
| `serde` + `serde_json` | Parse jj JSON output, GitHub API responses |
| `tokio` | Async runtime (required by octocrab) |
| `octocrab` | GitHub API client |
| `thiserror` | Error type definitions |
| `anyhow` | Error propagation with context |
| `inquire` | Interactive bookmark selection |
| `indicatif` | Progress bars/spinners |
| `miette` | User-facing error diagnostics (later milestone) |

## Conventions

### Rust

- Edition 2024.
- Use `cargo nextest run` for testing, not `cargo test`.
- Prefer `cargo run --bin jack` and `cargo build --bin jack` over `-p jack`.
- Find built binaries with:
  `cargo build --release --message-format json | jq -r 'select(.executable | . == null | not) | .executable'`
- **Never use `#[allow(...)]`**. Use `#[expect(..., reason = "...")]` instead,
  which requires a reason and warns when the expectation becomes unnecessary.

### Formatting

- `rustfmt.toml` uses nightly-only options (`format_strings`, `group_imports`,
  `imports_granularity`, `wrap_comments`, `doc_comment_code_block_width`).
- Run `mise run fmt:nightly` (or `cargo +nightly fmt --all`) for full
  formatting locally. CI uses stable `fmt:check` which silently ignores
  nightly-only options.
- If `mise` tools are missing from PATH after installation, run
  `mise install` to refresh.

### Version Control

- This repo uses `jj` (Jujutsu) for version control. Prefer `jj` over `git`.
- Before starting a new logical piece of work, verify a clean slate with
  `jj status`. If the current change is not empty, prompt the user or run
  `jj new`.
- Use `jj commit -m "message"` to finalize a change (describes and creates a
  new empty working copy in one step). Alternatively `jj describe -m` then
  `jj new`.
- Use `jj tug` to move the main bookmark forward to `@-` after committing.
- Push with `jj git push --bookmark main`.

### Error Handling

- Use `thiserror` for defining error enums in library code.
- Use `anyhow` for error propagation in application/CLI code.
- Provide context on errors: `thing.do_it().context("failed to do thing")?`.
- User-facing error messages should be clear and actionable.

### jj Interface

- Always run jj with `--config 'ui.paginate=never'`.
- Use `--template` for structured/JSON output where available.
- Define serde structs for every piece of jj output we consume.
- Paginate large output (100 items at a time) to avoid memory issues.

## Patterns Discovered

(This section is updated as we build. Record patterns, gotchas, and decisions
made during implementation here.)

- Use `#[expect(..., reason = "...")]` instead of `#[allow(...)]` — it warns
  when the suppressed lint no longer fires, preventing stale suppressions.
- When a field/method is dead in the bin target but used in tests, use
  `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]` to satisfy both
  `--all-targets` clippy and `-D warnings`.
- jj template strings for JSON output use NDJSON (one JSON object per line)
  with `\n` separator in the template. Parse with `lines()` + per-line
  `serde_json::from_str`.
- `jj git remote list` outputs plain text (`name url` per line), not JSON.
  Parse with simple string splitting.
- `trunk()` remote bookmarks include an internal `@git` entry — filter it
  out when detecting the default branch.
- When a bookmark is unsynced, `jj bookmark list` emits two entries (local
  and remote tracking target) with the same name. Deduplicate by keeping
  only the first entry per name.
- `jj abandon` reverts the working copy to match the parent. If you have
  uncommitted edits in the working copy, use `jj new` first or `jj undo`
  to recover.

## Decisions Log

(Record significant architectural or design decisions here with date and
rationale.)

- **2026-02-19**: `auth setup` instead of `auth help` — avoids ambiguity with
  clap's built-in `--help`.
- **2026-02-19**: `Option<Commands>` for no-subcommand — clean upgrade path to
  M6 interactive mode without removing a clap attribute.
- **2026-02-19**: `#[tokio::main]` from day one — tokio is a required
  dependency (octocrab). Adding the async runtime now avoids restructuring
  main.rs later.
- **2026-02-19**: `#[expect]` over `#[allow]` — requires a reason and warns
  when the expectation becomes unnecessary.
- **2026-02-19**: Generic `Jj<R: JjRunner>` over `dyn JjRunner` — avoids
  `async_trait` dependency, zero-cost dispatch, works with edition 2024's
  native async fn in traits.
- **2026-02-19**: NDJSON templates over array-based JSON — simpler parsing
  (line-by-line), natural pagination boundary, no need to accumulate large
  JSON arrays in memory.
