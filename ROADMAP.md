# Roadmap: jack

Incremental milestones for building jack, a Rust rewrite of jj-stack. Each
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

**Done when**: `jack --help`, `jack submit --help`, and `jack auth --help` all
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

**Done when**: Can run `jack` in a jj repo and have it list bookmarks and their
relationships without errors.

---

## Milestone 2: Change Graph Construction

Port the core graph-building algorithms from jj-stack.

- [ ] Define data types: `ChangeGraph`, `BookmarkSegment`, `BranchStack`
- [ ] Port `buildChangeGraph()`:
  - Iterate bookmarks
  - Traverse each toward trunk with pagination
  - Discover segments (groups of commits under each bookmark)
  - Build child→parent adjacency list
  - Detect and exclude merge commits (tainting)
  - Identify leaf bookmarks
- [ ] Port `groupSegmentsIntoStacks()`:
  - Walk from each leaf to root via adjacency list
  - Produce one `BranchStack` per leaf
- [ ] Port topological sort (Kahn's algorithm) for display ordering
- [ ] Tests matching jj-stack's test cases:
  - Simple linear stack (A → B → C)
  - Complex branching (shared ancestors, multiple children)
  - Merge commit exclusion
  - Multiple bookmarks on one change

**Done when**: Given a jj repo with bookmarks, `jack` correctly identifies all
stacks and can print them.

---

## Milestone 3: GitHub Authentication

- [ ] Resolve auth token in order:
  1. `gh auth token` (shell out to gh CLI)
  2. `GITHUB_TOKEN` env var
  3. `GH_TOKEN` env var
- [ ] Validate token via GitHub API (octocrab)
- [ ] `jack auth test` — prints success/failure with username
- [ ] `jack auth help` — prints setup instructions
- [ ] Clear error messages when auth fails

**Done when**: `jack auth test` reports the authenticated GitHub user.

---

## Milestone 4: Forge Trait & GitHub Implementation

Design a trait abstraction so the core logic is forge-agnostic.

- [ ] Define `Forge` trait:
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
- [ ] Implement `GitHubForge` using octocrab
- [ ] Stack comment formatting with base64-encoded metadata (compatible with
  jj-stack's format for migration)
- [ ] PR existence checking
- [ ] PR creation with title + body
- [ ] PR base branch updates

**Done when**: Can create a PR on GitHub from jack, with a stack comment.

---

## Milestone 5: Three-Phase Submission

Port the core submission workflow.

- [ ] **Phase 1 — Analysis** (`analyze_submission`):
  - Take a target bookmark and change graph
  - Find the stack containing that bookmark
  - Return relevant segments from trunk to target
- [ ] **Phase 2 — Planning** (`create_submission_plan`):
  - Check GitHub for existing PRs for each bookmark
  - Determine which bookmarks need pushing
  - Determine which PRs need creation
  - Determine which PR bases need updating
  - Report the plan to the user
- [ ] **Phase 3 — Execution** (`execute_submission_plan`):
  - Push bookmarks via `jj git push`
  - Update PR bases
  - Create new PRs (bottom to top)
  - Create/update stack comments on all PRs
- [ ] `jack submit <bookmark>` — full end-to-end submission
- [ ] `--dry-run` flag — show plan without executing
- [ ] `--remote` flag — specify which remote to push to (default: `origin`)
- [ ] Progress output during execution (indicatif)

**Done when**: Can run `jack submit my-bookmark` and have it push, create PRs,
set correct bases, and add stack comments — matching jj-stack's behavior.

---

## Milestone 6: Interactive Mode

Default behavior when `jack` is run with no arguments.

- [ ] Build change graph and display stacks as ASCII tree
- [ ] Interactive bookmark selection using `inquire`
- [ ] After selection, run the three-phase submission for that bookmark
- [ ] Handle the case where multiple bookmarks point to the same change

**Done when**: Running `jack` with no args shows stacks and lets the user pick
a bookmark to submit.

---

## Milestone 7: Polish & Quality of Life

- [ ] Parallel GitHub API calls where dependencies allow (check existing PRs
  concurrently)
- [ ] `--draft` flag for creating PRs as drafts
- [ ] PR body populated from full jj change descriptions (not just title)
- [ ] Configurable default branch detection (don't hardcode names)
- [ ] Better error messages with miette diagnostics
- [ ] Handle non-user bookmarks gracefully (filter, don't crash)

**Done when**: All known jj-stack issues are addressed in jack.

---

## Milestone 8: Extended Features

- [ ] GitHub Enterprise support (configurable API base URL in forge config)
- [ ] Fork workflow (push to fork remote, create PR against upstream)
- [ ] Stack comments showing PR titles instead of bookmark names
- [ ] Config file support (`.jack.toml` or similar) for per-repo settings:
  - Default remote
  - Default branch override
  - GitHub Enterprise URL
  - Draft PR default
- [ ] Second forge implementation (Forgejo or GitLab) using the forge trait

**Done when**: jack covers all common stacked-PR workflows across GitHub
configurations.

---

## Sidequest: Integration Test Harness

Binary-level integration tests that exercise the real `jack` binary against a
real `jj` repo (no mocks).

- [ ] Test harness that creates a temporary jj repo in `/tmp/` with `jj init`
- [ ] Fixture setup: create commits, bookmarks, bookmark stacks, synced and
  unsynced bookmarks (no remote needed initially — test local-only behavior)
- [ ] Run the compiled `jack` binary against the fixture repo and assert on
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
