# Roadmap: stakk

Incremental milestones for building stakk, a Rust rewrite of jj-stack. Each
milestone builds on the previous and produces something testable.

See [ANALYSIS.md](ANALYSIS.md) for the full research behind these decisions.

---

## Milestone 0: Project Skeleton

Set up the project structure and dependencies.

- [x] `Cargo.toml` with initial dependencies: clap, serde, serde_json, tokio,
  thiserror, anyhow
- [x] Basic clap CLI with subcommands: `submit`, `auth`
- [x] Error types module with thiserror
- [x] Module structure:
  ```
  src/
  ├── main.rs          # CLI entry point
  ├── cli/             # clap definitions
  ├── jj/              # jj CLI interface (all VCS ops go through here)
  ├── forge/           # forge trait + GitHub implementation
  ├── graph/           # change graph construction
  ├── submit/          # three-phase submission
  └── error.rs         # error types
  ```
  Note: no `git/` module. All git operations go through `jj` commands, which
  makes workspaces and non-colocated repos work automatically (see
  [ANALYSIS.md, section 7](ANALYSIS.md#7-workspaces-and-non-colocated-repos)).

**Done when**: `stakk --help`, `stakk submit --help`, and `stakk auth --help` all
print usage information (even outside a jj repo).

---

## Milestone 1: jj Interface Layer

Shell out to `jj` and parse JSON output into typed Rust structs.

- [x] Define serde structs for jj output: bookmarks, log entries, commit
  metadata
- [x] Implement functions:
  - `get_my_bookmarks()` — `jj bookmark list --revisions "mine() ~ trunk()"`
  - `get_branch_changes_paginated()` — `jj log` with pagination (100 at a
    time)
  - `get_git_remote_list()` — `jj git remote list` (also used to extract
    GitHub owner/repo from remote URL — no git library needed)
  - `get_default_branch()` — detect from trunk() remote bookmarks
  - `push_bookmark()` — `jj git push --bookmark <name>`
  - `git_fetch()` — `jj git fetch --all-remotes`
- [x] Helper for running jj commands with `ui.paginate=never` config
- [x] Remote URL parsing: extract owner/repo from HTTPS and SSH GitHub URLs
- [x] Tests with captured jj output fixtures

**Done when**: Can run `stakk` in a jj repo and have it list bookmarks and their
relationships without errors.

---

## Milestone 2: Change Graph Construction

Port the core graph-building algorithms from jj-stack.

- [x] Define data types: `ChangeGraph`, `BookmarkSegment`, `BranchStack`
- [x] Port `buildChangeGraph()`:
  - Iterate bookmarks
  - Traverse each toward trunk with pagination
  - Discover segments (groups of commits under each bookmark)
  - Build child→parent adjacency list
  - Detect and exclude merge commits (tainting)
  - Identify leaf bookmarks
- [x] Port `groupSegmentsIntoStacks()`:
  - Walk from each leaf to root via adjacency list
  - Produce one `BranchStack` per leaf
- [x] Port topological sort (Kahn's algorithm) for display ordering
- [x] Tests matching jj-stack's test cases:
  - Simple linear stack (A → B → C)
  - Complex branching (shared ancestors, multiple children)
  - Merge commit exclusion
  - Multiple bookmarks on one change

**Done when**: Given a jj repo with bookmarks, `stakk` correctly identifies all
stacks and can print them.

---

## Milestone 3: GitHub Authentication

- [x] Resolve auth token in order:
  1. `gh auth token` (shell out to gh CLI)
  2. `GITHUB_TOKEN` env var
  3. `GH_TOKEN` env var
- [x] Validate token via GitHub API (octocrab)
- [x] `stakk auth test` — prints success/failure with username
- [x] `stakk auth setup` — prints setup instructions
- [x] Clear error messages when auth fails

**Done when**: `stakk auth test` reports the authenticated GitHub user.

---

## Milestone 4: Forge Trait & GitHub Implementation

Design a trait abstraction so the core logic is forge-agnostic.

- [x] Define `Forge` trait:
  ```rust
  trait Forge {
      async fn get_authenticated_user(&self) -> Result<String>;
      async fn find_pr_for_branch(&self, head: &str) -> Result<Option<PullRequest>>;
      async fn create_pr(&self, params: CreatePrParams) -> Result<PullRequest>;
      async fn update_pr_base(&self, pr_number: u64, new_base: &str) -> Result<()>;
      async fn list_comments(&self, pr_number: u64) -> Result<Vec<Comment>>;
      async fn create_comment(&self, pr_number: u64, body: &str) -> Result<Comment>;
      async fn update_comment(&self, comment_id: u64, body: &str) -> Result<()>;
      async fn get_repo_default_branch(&self) -> Result<String>;
  }
  ```
- [x] Implement `GitHubForge` using octocrab
- [x] Stack comment formatting with base64-encoded metadata
- [x] PR existence checking
- [x] PR creation with title + body
- [x] PR base branch updates

**Done when**: Can create a PR on GitHub from stakk, with a stack comment.

---

## Milestone 5: Three-Phase Submission

Port the core submission workflow.

- [x] **Phase 1 — Analysis** (`analyze_submission`):
  - Take a target bookmark and change graph
  - Find the stack containing that bookmark
  - Return relevant segments from trunk to target
- [x] **Phase 2 — Planning** (`create_submission_plan`):
  - Check GitHub for existing PRs for each bookmark
  - Determine which bookmarks need pushing
  - Determine which PRs need creation
  - Determine which PR bases need updating
  - Report the plan to the user
- [x] **Phase 3 — Execution** (`execute_submission_plan`):
  - Push bookmarks via `jj git push`
  - Update PR bases
  - Create new PRs (bottom to top)
  - Create/update stack comments on all PRs
- [x] `stakk submit <bookmark>` — full end-to-end submission
- [x] `--dry-run` flag — show plan without executing
- [x] `--remote` flag — specify which remote to push to (default: `origin`)
- [x] Progress output during execution (indicatif)

**Done when**: Can run `stakk submit my-bookmark` and have it push, create PRs,
set correct bases, and add stack comments — matching jj-stack's behavior.

---

## Milestone 6: Polish & Quality of Life

- [x] Write a comprehensive README (project overview, installation placeholder,
  usage examples, how stacking works, comparison with jj-stack)
- [x] Concurrent PR lookups, base updates, and comment operations during
  Phase 2 & 3 (using `futures::future::join_all`)
- [x] Progress output during Phase 1 & 2 (indicatif spinner with status
  messages for auth, remote, graph, analysis, and PR lookups)
- [x] `--draft` flag for creating PRs as drafts
- [x] PR body populated from full jj change descriptions (not just title).
  Single commit: body is everything after the title line. Multiple commits:
  descriptions joined with `---` separators. Only set on creation (don't
  overwrite manually-edited PR bodies).
- [x] Configurable default branch detection — already done since M2;
  `get_default_branch()` uses `trunk()` revset, not hardcoded names.
- [x] Better error messages with miette diagnostics (`Diagnostic` derives on
  `JjError`, `AuthError`, `ForgeError`, `StakkError`; help text on actionable
  variants; `main()` extracts and prints diagnostic help)
- [x] Handle non-user bookmarks gracefully (filter `local_bookmark_names` to
  only user-owned bookmarks during traversal; non-user bookmarks don't create
  segment boundaries)
- [x] Upgraded all dependencies to latest versions (including indicatif 0.18)
- [x] Added `futures` and `miette` dependencies

**Done when**: README is polished and publishable, all known jj-stack issues
are addressed in stakk.

---

## Milestone 7: Interactive Mode

Interactive bookmark selection when `stakk submit` is run without a bookmark
argument. See [RESEARCH-interactive-selector.md](RESEARCH-interactive-selector.md)
for full research and trade-off analysis.

### Part 1: Basic interactive selection — DONE (WIP commit `5e4c72e8`)

- [x] `stakk show` subcommand extracted (default when no subcommand given)
- [x] `bookmark` argument made optional in `SubmitArgs`
- [x] `NotInteractive` and `PromptCancelled` error variants
- [x] `src/select.rs` module with graph selector using `console` crate
- [x] `○`/`│` graph characters, green focused, red ancestors, dim commits
- [x] Keyboard navigation (arrows/jk, Enter, Esc/q)
- [x] Auto-select when only one bookmark exists
- [x] TTY detection with actionable error message
- [x] `CursorGuard` scope guard for cursor restoration
- [x] 6 pure-function tests for `collect_selectable_bookmarks()`

### Part 2: Viewport windowing — TODO

The current renderer writes all lines at once. When the graph exceeds
terminal height, it scrolls past the top and becomes unusable.

**Approach: Console + viewport windowing (Option A from research)**

No new dependencies. Refactor `select.rs`:

- [ ] Pre-render graph into `Vec<GraphLine>` (text + bookmark_index +
  line_type), separating data from rendering
- [ ] `render_viewport()` replaces `render_graph()`: uses `Term::size()`
  to get terminal height, only renders the visible window of lines
- [ ] `calculate_scroll_offset()` pure function: keeps focused bookmark
  visible, avoids unnecessary jumping (follows jj-stack's pattern)
- [ ] Scroll indicators (`▲ N more` / `▼ N more`) when content overflows
- [ ] `selectable_indices: Vec<usize>` — navigation skips non-selectable
  rows (connector lines, commit lines, separators)
- [ ] Tests for `calculate_scroll_offset()` (fits/above/below/stable)
- [ ] Tests for `build_graph_lines()` (line sequence, selectable indices)

**Done when**: `stakk submit` (no args) in `../jack-testing/` shows a
viewport-clipped graph that scrolls as the user navigates, with the focused
bookmark always visible.

### Part 3: DAG graph rendering — TODO

Replace the linear per-stack display with jj-stack-style column-based
graph layout showing true branching structure.

**Approach: Column-based layout adapted from jj-stack**

Uses existing `ChangeGraph` fields (`adjacency_list`, `segments`,
`stack_leaves`). No new dependencies.

- [ ] Topological sort (leaf-to-root, Kahn's algorithm on `stack_leaves`)
- [ ] Column tracking: `Vec<Option<String>>` of active columns per change
- [ ] Branching characters: `○` (node), `│` (continuation), `├` (branch),
  `─╯` (merge converging), `─│` (horizontal crossing vertical)
- [ ] Show change ID + bookmark name per row (like jj-stack)
- [ ] Stacks sharing ancestors merge visually in the graph
- [ ] Tests for column layout (linear, branching, merging)

**Done when**: `stakk submit` (no args) shows a graph matching jj-stack's
visual style, with branching indentation for stacks that share ancestors.

### Alternative approaches considered (see research doc)

- **Option B (Ratatui)**: `Viewport::Inline` + `List` widget for built-in
  scrolling. Adds ratatui+crossterm deps. Better resize handling, but
  potential conflict with console crate.
- **Option C (Inquire)**: Two-step `Select` prompts (pick stack, then
  bookmark). Minimal code (~50 lines), auto-viewport, type-to-filter. But
  loses graph visualization entirely.

---

## Milestone 8: Extended Features

- [ ] GitHub Enterprise support (configurable API base URL in forge config)
- [ ] Fork workflow (push to fork remote, create PR against upstream)
- [ ] Stack comments showing PR titles instead of bookmark names (low priority
  — GitHub already renders PR links nicely in comments)
- [ ] Config file support (`.stakk.toml` or similar) for per-repo settings:
  - Default remote
  - Default branch override
  - GitHub Enterprise URL
  - Draft PR default
- [ ] Second forge implementation (Forgejo or GitLab) using the forge trait

**Done when**: stakk covers all common stacked-PR workflows across GitHub
configurations.

---

## Sidequest: Replace anyhow with concrete error types

Remove `anyhow` from non-`main` code. Currently `submit/mod.rs` uses
`anyhow::Result` with `.context()` for three public functions
(`analyze_submission`, `create_submission_plan`, `execute_submission_plan`).
Replace with concrete `thiserror` + `miette::Diagnostic` error types.

- [x] Define `SubmitError` enum in `submit/mod.rs` with variants for each
  failure mode: bookmark not found, segment has no bookmark name, forge
  errors (`#[from] ForgeError`), jj push errors (`#[from] JjError`), comment
  errors. Derive `Diagnostic` with actionable help text.
- [x] Replace `anyhow::Result` returns in `analyze_submission`,
  `create_submission_plan`, and `execute_submission_plan` with
  `Result<T, SubmitError>`.
- [x] In `main.rs`, `run()` returns `Result<(), StakkError>`. `main()`
  converts errors into `miette::Report` at the boundary. This replaces the
  manual `print_diagnostic_help` function — miette's report renderer
  automatically walks `diagnostic_source()` and renders help, codes, etc.
  from every level in the chain.
- [x] Remove `anyhow` from `Cargo.toml` — zero anyhow usage anywhere.
- [x] Verify `#[diagnostic(transparent)]` on `StakkError` variants correctly
  forwards diagnostic metadata from inner errors through the full chain.

**Pattern**: Concrete error types (`thiserror` + `Diagnostic`) all the way
up; `miette::Report` only at the `main()` boundary. This gives the best of
both worlds: matchable/dowcastable errors in library code, rich CLI
diagnostics at the top level.

**Goal**: Zero `anyhow` usage. Every error is a concrete type with
`Diagnostic` metadata. `main() -> miette::Result<()>` renders everything
automatically.

---

## Sidequest: Integration Test Harness

Binary-level integration tests that exercise the real `stakk` binary against a
real `jj` repo (no mocks).

- [ ] Test harness that creates a temporary jj repo in `/tmp/` with `jj init`
- [ ] Fixture setup: create commits, bookmarks, bookmark stacks, synced and
  unsynced bookmarks (no remote needed initially — test local-only behavior)
- [ ] Run the compiled `stakk` binary against the fixture repo and assert on
  stdout/stderr
- [ ] Teardown: clean up the temp directory after tests
- [ ] Integrate into `cargo nextest run` (as integration tests in `tests/`)

**Goal**: Catch regressions in the actual binary behavior that unit tests with
mock runners can't detect (template strings, jj CLI interface changes, output
formatting, etc.).

---

## Design Principles

Throughout all milestones:

1. **Shell out to jj, don't link jj-lib** — the CLI is the stable interface.
2. **Never call git directly** — all git operations go through `jj` commands.
   This makes workspaces and non-colocated repos work automatically.
3. **Forge trait from day one** — GitHub is first, but the abstraction is there.
4. **Idempotent operations** — re-running is always safe.
5. **Test with fixtures** — capture real jj/GitHub output as test data.
6. **Minimum viable at each milestone** — each milestone is usable on its own.
7. **Boring solutions** — prefer simple, obvious code over clever abstractions.
