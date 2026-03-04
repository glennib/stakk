# CLAUDE.md — stakk

> **IMPORTANT**: Agents MUST update this file as part of every planning step.
> This includes: recording progress on milestones, documenting new patterns or
> conventions discovered during development, updating principles when they
> evolve, noting architectural decisions made during implementation, and keeping
> the current status accurate. Treat this file as the living source of truth for
> how we build stakk.

## Project Overview

**stakk** is a Rust rewrite of [jj-stack](https://github.com/keanemind/jj-stack)
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
- **Milestone 2 (Change Graph Construction)**: Complete — `ChangeGraph`,
  `BookmarkSegment`, `BranchStack` types, `build_change_graph()` with
  paginated traversal, merge-commit tainting, `topological_sort()`,
  14 unit/integration tests, CLI displays stacks.
- **Milestone 3 (GitHub Authentication)**: Complete — `auth::resolve_token()`
  with priority cascade (gh CLI → GITHUB_TOKEN → GH_TOKEN), `stakk auth test`
  validates token and prints username, `stakk auth setup` prints instructions,
  4 unit tests.
- **Milestone 4 (Forge Trait & GitHub Implementation)**: Complete — `Forge`
  trait with 8 async methods, `GitHubForge` implementation using octocrab,
  stack comment formatting with base64-encoded metadata,
  11 comment formatting tests.
- **Milestone 5 (Three-Phase Submission)**: Complete —
  `analyze_submission()` (Phase 1, pure function), `create_submission_plan()`
  (Phase 2, queries forge), `execute_submission_plan()` (Phase 3, pushes,
  creates/updates PRs, manages stack comments). `--dry-run` flag prints plan
  without executing, `--remote` flag overrides push remote. `indicatif`
  spinner for progress output. 15 new tests (5 Phase 1, 5 Phase 2, 5 Phase 3),
  77 total tests.
- **Milestone 6 (Polish & QoL)**: Complete — `--draft` flag, PR body from
  descriptions, concurrent API calls, progress spinners, non-user bookmark
  filtering, miette diagnostics, dependency upgrades, README. 85 total tests.
- **Sidequest (Replace anyhow)**: Complete — `SubmitError` enum with
  `Diagnostic` derives, `StakkError` aggregates all error types, `main()`
  uses `miette::Report` for rendering, zero `anyhow` usage.
- **Ratatui TUI selector**: Complete — replaced `inquire` two-stage prompts
  with a ratatui inline-viewport TUI. Screen 1 renders a tree graph of all
  branch stacks with `○`/`◆` nodes and Unicode box-drawing edges; user
  navigates with arrow keys or j/k to select a leaf branch. Screen 2 shows
  commits on the selected trunk→leaf path with checkboxes; existing bookmarks
  are pre-checked, unmarked commits can be toggled to get auto-generated
  `stakk-<change_id>` bookmarks. New bookmarks are created via
  `jj bookmark create` before submission. `Jj::create_bookmark()` is the
  first write operation stakk performs on jj state. 117 total tests.
- **Stack comment templating**: Complete — minijinja-based templating for
  stack comments. Default template uses table layout with PR URLs, bookmark
  names, and base branches. Custom templates via `--template` flag or
  `STAKK_TEMPLATE` env var. `StackCommentContext`/`StackEntryContext` structs
  provide rich template context. Metadata line always prepended
  programmatically (not part of template). 124 total tests.
- **Environment variable config**: Complete — `--draft` (`STAKK_DRAFT`),
  `--remote` (`STAKK_REMOTE`), and `--template` (`STAKK_TEMPLATE`) are all
  configurable via environment variables. CLI flags override env vars.

## Testing

- **Unit/integration tests**: `cargo nextest run --all-targets` (124 tests).
- **Manual testing repo**: `../jack-testing/` (github.com/glennib/jack-testing).
  A jj repo with pre-built bookmark stacks for end-to-end verification.
  Run stakk from within that directory to test against real jj output.

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

### 8. No jj-stack compatibility

jj-stack compatibility is explicitly a non-goal. stakk uses its own comment
metadata format (`STAKK_STACK` prefix), its own serde field naming (snake_case),
and its own comment footer. Do not reference jj-stack's format, data
structures, or conventions in code or documentation.

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

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` (v4, derive) | CLI framework |
| `serde` + `serde_json` | Parse jj JSON output, GitHub API responses |
| `tokio` | Async runtime (required by octocrab) |
| `octocrab` | GitHub API client |
| `base64` | Stack comment metadata encoding |
| `http` | HTTP status codes for error mapping |
| `thiserror` | Error type definitions |
| `miette` | User-facing error diagnostics (`Diagnostic` derives) |
| `minijinja` | Jinja2 templating for stack comments |
| `ratatui` | TUI framework for interactive graph/bookmark selection |
| `crossterm` | Terminal events and raw mode for TUI input handling |
| `console` | Terminal I/O (used by indicatif) |
| `futures` | Concurrent async operations (`join_all`) |
| `indicatif` | Progress bars/spinners |
| `rand` | Fallback bookmark name generation |

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
  formatting locally. CI uses stable `fmt:check` which silently ignores
  nightly-only options.
- **Always run `cargo +nightly fmt --all` before committing.**
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

- Use `thiserror` + `miette::Diagnostic` for defining error enums everywhere.
- Concrete error types all the way up; `miette::Report` only at the `main()`
  boundary for rendering.
- No `anyhow` — every error is a concrete type with `Diagnostic` metadata.
- User-facing error messages should be clear and actionable.
- Use `#[diagnostic(help(...))]` on unit/tuple variants for actionable advice.
  For struct variants with named fields, embed advice in `#[error(...)]` to
  avoid false-positive `unused_assignments` warnings from the macro.

### jj Interface

- Always run jj with `--config 'ui.paginate=never'`.
- Use `--template` for structured/JSON output where available.
- Define serde structs for every piece of jj output we consume.
- Paginate large output (100 items at a time) to avoid memory issues.

## Workflow

### Starting a milestone

- Before planning, summarize the milestone requirements from ROADMAP.md to the
  user so they can confirm scope before any planning agent is launched.

### Completing a milestone

- Mark all checklist items as `[x]` in ROADMAP.md.
- Present the user with a summary: what was built, what was tested, and why the
  "done when" criteria are satisfied.
- Update the "Current Status" section in this file.

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
- Graph traversal uses `"trunk()"` as the revset base (not the branch name
  like `"main"`). This lets jj resolve the trunk commit automatically
  regardless of what the default branch is called.
- The `Forge` trait uses `impl Future` in trait (edition 2024), same as
  `JjRunner`. No `async_trait` dependency needed.
- octocrab treats PR comments as issue comments — use `issues().list_comments()`
  and `issues().create_comment()` for PR comment operations.
- octocrab's `pulls().create()` returns a builder that borrows the pulls
  handler. Bind the handler to a variable (`let pulls = ...`) before calling
  `.create()` to avoid temporary lifetime issues.
- `CommentId` in octocrab is a newtype around `u64`. Use `CommentId::from(id)`
  and `.into_inner()` to convert.
- `resolve_token()` does NOT validate the token. Validation happens separately
  via `Forge::get_authenticated_user()`. This keeps resolution fast (no
  network call) and testable.
- `try_gh_cli()` returns `Ok(None)` for "gh not installed" and "gh not
  authenticated" — both are expected fallthrough cases, not errors.
- Stack comment metadata uses `STAKK_STACK` prefix (not jj-stack's prefix).
  jj-stack compatibility is not a goal.
- Three-phase submission (analyze → plan → execute) keeps business logic
  testable with mock `Forge` and `JjRunner`. `main.rs` is the composition
  root that creates concrete `GitHubForge` and `Jj<RealJjRunner>`, then
  passes them as `&F: Forge` and `&Jj<R>` to generic phase functions.
- Mock state shared between test code and mock impls uses
  `Arc<Mutex<Vec<...>>>` since `Jj::new()` takes ownership of the runner.
  Create the Arc before constructing the mock, clone it, and inspect
  after execution.
- `resolve_github_remote()` stays in `main.rs` — it's CLI orchestration
  that creates a `Jj<RealJjRunner>` internally and is not part of the
  submission logic.
- `futures::future::join_all` for concurrent forge operations — simpler than
  `FuturesUnordered`, sufficient for small numbers of concurrent calls.
- `build_pr_body()` handles single-commit (strip title) and multi-commit
  (join with `---`) cases. Returns `None` for empty/title-only descriptions.
- PR body is only set on creation, not on update — avoids overwriting
  manually-edited PR bodies.
- Non-user bookmarks on commits are filtered during traversal using a
  `HashSet<String>` of user bookmark names. This prevents spurious segment
  boundaries from bookmarks belonging to other users.
- miette `#[diagnostic(help(...))]` on struct variants with named fields
  causes false-positive `unused_assignments` warnings from the macro
  expansion. Workaround: embed actionable text in the `#[error(...)]`
  message directly for field-based variants; use `#[diagnostic(help(...))]`
  only on unit or tuple variants.
- `main()` converts `StakkError` to `miette::Report` for rendering. miette's
  graphical report handler walks `diagnostic_source()` automatically to
  show help from any error in the chain (e.g. `SubmitError::PushFailed`
  wrapping `JjError::NotFound` with its help text).
- `SubmitError` uses `#[source]` on `ForgeError`/`JjError` fields — miette
  automatically treats `#[source]` fields that implement `Diagnostic` as
  diagnostic sources, walking the chain to render help from inner errors.
- ratatui inline viewport (`Viewport::Inline(height)`) renders within
  terminal output, no fullscreen. Use `crossterm::terminal::size()` to
  cap viewport height. Call `enable_raw_mode()` before and
  `disable_raw_mode()` after.
- crossterm's `event::read()` blocks synchronously — runs on main thread
  before async submission phases, no need for async event handling.
- Graph layout deduplicates shared segments across stacks by `commit_id`
  (not `change_id`, since all commits in a segment share the same
  `change_id`). Shared commits appear once, in the first stack's column.
- Auto-generated bookmark names use `stakk-<first 12 chars of change_id>`,
  matching jj's `push-<change_id>` convention for deterministic naming.
- TTY detection uses `std::io::stdin().is_terminal()` (Rust 1.70+ stdlib)
  instead of relying on a TUI library's detection.
- `Jj::create_bookmark()` is the first write operation on jj state —
  calls `jj bookmark create <name> -r <revision>`. After creating new
  bookmarks, the change graph must be rebuilt to include them.

- minijinja `Environment::add_template` requires `&'source str` matching
  the environment's lifetime. For custom templates (owned `String`), use
  `Box::leak(source.into_boxed_str())` to get a `&'static str`. This is
  fine since the env is created once per invocation.
- Stack comment metadata line (`<!--- STAKK_STACK: ... --->`) is always
  prepended programmatically — it is NOT part of the minijinja template.
  This ensures metadata survives custom templates that might omit it.
- `StackCommentContext` / `StackEntryContext` are separate from the lean
  `StackCommentData` / `StackEntry` types. The former are rich rendering
  contexts (title, base, is_current, position); the latter are minimal
  machine-readable metadata embedded as base64 JSON.
- `format_stack_comment` returns `Result` (not infallible) because user
  templates can fail to render. Default template is validated at compile
  time via `include_str!` + tests.

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
- **2026-02-19**: `Forge` trait uses `impl Future` in trait, same pattern
  as `JjRunner` — zero-cost dispatch, no `async_trait` crate needed.
- **2026-02-19**: `auth::resolve_token()` as standalone function, not a
  struct — no state to carry, matches "boring solutions" principle.
- **2026-02-19**: `GitHubForge::new()` creates the octocrab client
  internally — caller provides token + owner/repo, not an octocrab instance.
- **2026-02-19**: Forge-agnostic types (`PullRequest`, `Comment`,
  `CreatePrParams`) in `forge/mod.rs` alongside the trait — `GitHubForge`
  maps between octocrab types and these.
- **2026-02-19**: jj-stack compatibility is explicitly a non-goal. stakk uses
  its own `STAKK_STACK` comment prefix and snake_case serde fields.
- **2026-02-19**: Minimal `submit_bookmark()` wiring in main.rs for M4 —
  temporary scaffolding replaced by full three-phase submission in M5.
- **2026-02-19**: Three-phase submission uses `anyhow::Result` (not
  `thiserror`) — application-level orchestration code, errors from
  sub-systems are wrapped with `.context()`.
- **2026-02-19**: `SubmissionAnalysis` does not carry `target_bookmark` —
  the field was unused after construction, removed to avoid dead_code warning.
- **2026-02-19**: `needs_push` is always `true` in M5 — always pushing is
  safe and idempotent. Optimization to skip synced bookmarks deferred.
- **2026-02-19**: `--remote` flag serves dual purpose — selects which
  remote jj pushes to AND which remote URL resolves the GitHub owner/repo.
  `resolve_github_remote(Some("name"))` validates it's a GitHub URL.
- **2026-02-19**: PR body only set on creation — `execute_submission_plan`
  passes `bp.body.clone()` to `CreatePrParams` but does not update the body
  of existing PRs. This prevents overwriting user-edited PR descriptions.
- **2026-02-19**: `--draft` threaded via `SubmissionPlan.draft` — stored
  once on the plan, read during PR creation. Simpler than per-bookmark draft.
- **2026-02-19**: Concurrent API calls use `join_all` not `FuturesUnordered`
  — simpler, sufficient for the small number of concurrent operations.
- **2026-02-19**: miette at presentation layer only — `Diagnostic` derived
  on error enums, but `anyhow` remains for propagation. `main()` extracts
  help from the root cause via `downcast_ref`.
- **2026-02-19**: `run()` extracted from `main()` — `main()` converts errors
  to `miette::Report` for display, `run()` returns `Result<(), StakkError>`.
- **2026-02-19**: Zero `anyhow` — concrete error types (`thiserror` +
  `Diagnostic`) all the way up. `SubmitError` in `submit/mod.rs` with
  per-variant context (bookmark name, PR number). `StakkError` in `error.rs`
  aggregates all error types with `#[diagnostic(transparent)]`. Remote
  resolution errors (`RemoteNotGithub`, `RemoteNotFound`, `NoGithubRemote`)
  added to `StakkError` to replace `anyhow::bail!()` in `main.rs`.
- **2026-02-19**: Renamed from `jack` to `stakk` — the name `jack` was
  already taken on crates.io. `stakk` reflects the stacking concept and is
  available. All references updated: crate name, binary name, error types
  (`JackError` → `StakkError`), comment metadata prefix (`JACK_STACK` →
  `STAKK_STACK`), documentation, and user-facing strings.
- **2026-02-20**: `stakk show` extracted as subcommand — `Show` variant in
  `Commands`, default when no subcommand given (`Some(Commands::Show) | None`).
  Makes room for swapping the default to interactive submit later.
- **2026-02-20**: Bookmark argument optional on `stakk submit` — `Option<String>`
  in `SubmitArgs`. When `None`, interactive selection is triggered after
  spinner finishes (graph is already built). Spinner is split: first spinner
  covers auth/remote/graph, cleared before interactive prompt, second spinner
  covers submission phases.
- **2026-02-20**: `inquire` over custom `console` renderer for interactive
  bookmark selection — custom graph renderer couldn't handle viewport overflow
  (scrolls past terminal height with many bookmarks). `inquire::Select`
  provides built-in pagination, type-to-filter, and TTY detection in ~200
  lines vs 400+. Two-stage UX (stack → bookmark) replaces flat graph with
  relationship annotations for shared ancestors.
- **2026-02-20**: `inquire` with `default-features = false, features = ["console"]`
  — reuses stakk's existing `console = "0.16"` dependency (shared with
  `indicatif`). Avoids pulling in `crossterm` backend.
- **2026-02-20**: Single-bookmark case uses `inquire::Confirm` instead of
  auto-submitting — prevents accidental submission when running `stakk`
  with no arguments.
- **2026-02-20**: Default command changed from `show` to `submit` — running
  `stakk` without a subcommand now launches interactive submit. `stakk show`
  remains available as an explicit subcommand.
- **2026-03-01**: ratatui replaces inquire for interactive selection — two-screen
  TUI (graph view → bookmark assignment) with inline viewport. ratatui provides
  visual graph rendering that inquire's flat list couldn't. crossterm added as
  direct dep for `event::read()`.
- **2026-03-01**: `resolve_bookmark_interactively()` returns
  `SelectionResult` (with `Vec<BookmarkAssignment>`) instead of
  `Option<String>` — enables creating new bookmarks on unmarked commits.
  Each assignment carries `is_new` flag to indicate whether `jj bookmark
  create` is needed.
- **2026-03-01**: Graph layout deduplicates by `commit_id`, not
  `change_id` — all commits in a segment share the same `change_id`,
  but each has a unique `commit_id`. Dedup by commit_id prevents
  collapsing multi-commit segments while still merging shared segments
  across stacks.
- **2026-03-01**: `rand` added but not yet actively used — primary bookmark
  naming strategy is deterministic (`stakk-<change_id_prefix>`). `rand` is
  a fallback for name collisions.
- **2026-03-04**: minijinja for stack comment templating — replaces
  hardcoded numbered list with table layout. `--template` / `STAKK_TEMPLATE`
  for custom templates. Metadata line always prepended outside the template.
  `format_stack_comment` now returns `Result` since user templates can fail.
- **2026-03-04**: `STAKK_REMOTE` and `STAKK_DRAFT` env vars — `--remote`
  and `--draft` can now be configured via environment variables using clap's
  `env` attribute. CLI flags override env vars. `--dry-run` intentionally
  excluded — it's a one-off decision that would be surprising as a default.
