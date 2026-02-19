# Analysis: jj-stack and the Rust Rewrite (jack)

This document captures a thorough analysis of
[jj-stack](https://github.com/keanemind/jj-stack) — what it does, how it
works, where it falls short, and what the Rust ecosystem offers for building
its successor.

## References

- **Upstream repo**: <https://github.com/keanemind/jj-stack>
- **Fork** (used as primary reference): <https://github.com/glennib/jj-stack>
- **Upstream issues**: <https://github.com/keanemind/jj-stack/issues>
- **Upstream PRs**: <https://github.com/keanemind/jj-stack/pulls>
- **Jujutsu CLI reference**: <https://jj-vcs.github.io/jj/latest/cli-reference/>
- **Jujutsu architecture docs**: <https://jj-vcs.github.io/jj/latest/technical/architecture/>

## 1. What jj-stack Does

jj-stack is a CLI tool that bridges Jujutsu (jj) bookmarks to GitHub stacked
pull requests. It is **not** a wrapper around jj — it complements jj by taking
the local repo state (bookmarks pointing at changes) and turning it into a
coherent set of GitHub PRs that merge into each other in the correct order.

### Commands

- **`jst`** (no args) — Interactive mode. Builds a graph of all bookmarked
  changes, displays them as an ASCII tree, and lets the user select a bookmark
  to submit.
- **`jst submit <bookmark>`** — Pushes the bookmark and all its ancestors to
  the remote, creates or updates PRs, and maintains stack-awareness comments on
  each PR.
- **`jst auth test`** — Validates GitHub authentication.
- **`jst auth help`** — Shows how to set up auth.

### Authentication

Auth is resolved in order:
1. `gh auth token` (GitHub CLI)
2. `GITHUB_TOKEN` environment variable
3. `GH_TOKEN` environment variable

## 2. Core Data Model

### ChangeGraph

The central data structure (`src/lib/jjTypes.ts`). Contains:

- **`bookmarkedChangeAdjacencyList`**: `Map<ChangeId, ChangeId>` — directed
  edges from child bookmark to parent bookmark (toward trunk).
- **`stackLeafs`**: `Set<ChangeId>` — leaf nodes (bookmarks with no children).
  Each leaf defines one stack.
- **`bookmarkedChangeIdToSegment`**: `Map<ChangeId, BookmarkSegment>` — maps
  each bookmarked change to its segment (the group of commits under that
  bookmark).
- **`taintedChangeIds`**: `Set<ChangeId>` — merge commits and their
  descendants, excluded from stacking.

### BookmarkSegment

A group of consecutive commits that belong to a single bookmark. Fields:

- `bookmarkNames`: string[] — one or more bookmark names pointing at this
  change.
- `commits`: array of commit metadata (id, change_id, description, author,
  timestamp).
- `changeId`: the change ID this segment belongs to.

### BranchStack

A complete path from trunk to a leaf bookmark. An array of `BookmarkSegment`s
in bottom-to-top order. One stack exists per leaf in the graph.

## 3. Key Algorithms

### 3.1 Building the Change Graph

**Location**: `src/lib/jjUtils.ts`, `buildChangeGraph()` (lines 445–599)

**Steps**:

1. **Discover bookmarks**: Run `jj bookmark list --revisions "mine() ~ trunk()"`
   to get the current user's bookmarks that are not on trunk.

2. **For each bookmark**, call `traverseAndDiscoverSegments()`:
   - Walk backward from the bookmark toward trunk using
     `jj log -r '<bookmark>::'` with pagination (100 commits at a time).
   - At each commit, check if it has a bookmark. If so, record a new segment.
   - Stop when reaching a commit already collected by a previous traversal, or
     when reaching trunk.
   - Detect merge commits (>1 parent) and mark them and their descendants as
     "tainted" — excluded from stacking.

3. **Build adjacency list**: For each segment, its parent is the next
   bookmarked change closer to trunk. This creates directed child→parent edges.

4. **Identify leaves**: Any bookmarked change that no other bookmark points to
   as a parent is a leaf.

5. **Group into stacks**: `groupSegmentsIntoStacks()` walks from each leaf back
   to the root via the adjacency list, collecting the full path as a stack.

### 3.2 Traversal with Pagination

**Location**: `traverseAndDiscoverSegments()` in `jjUtils.ts`

The traversal fetches commits 100 at a time from jj. This handles large stacks
without memory issues. The pagination template includes commit metadata (id,
change_id, description, parents, bookmarks) formatted as JSON.

Key edge cases handled:
- A commit with multiple bookmarks → single segment with multiple bookmark
  names.
- A commit already seen in a previous bookmark's traversal → stop and connect.
- A merge commit → taint it and all descendants.

### 3.3 Topological Sort for Display

**Location**: `src/cli/AnalyzeCommand.res` (lines 51–89)

Uses Kahn's algorithm:
1. Calculate in-degrees from the adjacency list.
2. Start from leaves (in-degree 0), sorted by commit time.
3. Process each node, decrementing parent in-degrees.
4. When a parent's in-degree reaches 0, add it to the queue.

Result: bookmarks ordered leaves-first, trunk-last — suitable for display.

### 3.4 Three-Phase Submission

**Location**: `src/lib/submit.ts`

**Phase 1 — Analysis** (`analyzeSubmissionGraph`, lines 102–124):
- Takes a target bookmark and the change graph.
- Finds the stack containing that bookmark.
- Returns the path from trunk to the target as `relevantSegments`.

**Phase 2 — Planning** (`createSubmissionPlan`, lines 492–566):
- For each bookmark in the relevant segments, checks GitHub for an existing PR.
- Validates that existing PRs have the correct base branch (e.g., the previous
  bookmark in the stack, or `main` for the bottom).
- Produces a plan with three categories:
  - **Pushes needed**: bookmarks that need `jj git push`.
  - **New PRs needed**: bookmarks without existing PRs.
  - **Base updates needed**: existing PRs whose base branch is wrong (stack
    reordering happened).

**Phase 3 — Execution** (`executeSubmissionPlan`, lines 572–767):
- Pushes bookmarks (in order).
- Updates PR bases for existing PRs with wrong bases.
- Creates new PRs (bottom to top in the stack).
- Creates or updates a stack comment on every PR in the stack.

### 3.5 Stack Comments

**Location**: `submit.ts`, lines 323–377

Each PR in a stack gets a comment listing all PRs with links. The comment body
includes base64-encoded JSON metadata so that future runs can identify and
update the same comment rather than creating duplicates.

Format:
```
### Stack

1. #42 ← **this PR**
2. #41
3. #40 (merged)

<!-- base64-encoded metadata -->
```

### 3.6 PR Base Branch Determination

**Location**: `submit.ts`, `getBaseBranchOptions()` (lines 226–249)

- If the bookmark is the first (bottom) in the stack → base on the default
  branch (`main`/`master`/`trunk`).
- Otherwise → base on the previous bookmark's name (which is the head branch
  of the previous PR in the stack).

## 4. Strengths

- **Clean separation of concerns**: The three-phase design (analyze → plan →
  execute) makes the logic testable and debuggable.
- **Idempotent**: Re-running `submit` updates existing PRs rather than creating
  duplicates. Stack comments are identified by embedded metadata.
- **Merge-aware**: Deliberately excludes merge commits from stacking, avoiding
  confusing PR structures.
- **Multiple bookmarks per change**: Handles the case where several bookmarks
  point at the same commit.
- **Segment-based model**: Groups commits under bookmarks rather than treating
  each commit individually, matching how jj users think about their work.
- **Handles already-merged PRs**: Stack comments gracefully show which PRs in
  the stack have been merged.

## 5. Weaknesses and Known Issues

### Functional gaps

| Issue | Description | Upstream |
|-------|-------------|----------|
| No workspace support | Fails in jj workspaces — called `git` directly | [#19](https://github.com/keanemind/jj-stack/issues/19) |
| No non-colocated support | Requires `.git` directory; non-colocated repos have none | — |
| Hardcoded default branches | Only looks for `main`, `master`, `trunk` | [#12](https://github.com/keanemind/jj-stack/issues/12) |
| No draft PR support | Can't create PRs as drafts | [#5](https://github.com/keanemind/jj-stack/issues/5) |
| No fork workflow | Assumes push access to upstream | [#8](https://github.com/keanemind/jj-stack/issues/8) |
| No GitHub Enterprise | Hardcoded to github.com | [PR #13](https://github.com/keanemind/jj-stack/pull/13) |
| Bookmark names in comments | Stack comments show bookmark names, not PR titles | [#6](https://github.com/keanemind/jj-stack/issues/6) |
| No PR body content | Only sets PR title, body is empty | [PR #15](https://github.com/keanemind/jj-stack/pull/15) |
| Parser errors | "Unexpected token '<'" in some cases | [#18](https://github.com/keanemind/jj-stack/issues/18) |
| `--help` needs repo | Help command fails outside a jj repo | [#9](https://github.com/keanemind/jj-stack/issues/9) |
| Non-user bookmark crash | Crashes when other users' bookmarks exist | [#7](https://github.com/keanemind/jj-stack/issues/7) |
| No alternative platforms | GitHub only, no Forgejo/GitLab | [#1](https://github.com/keanemind/jj-stack/issues/1) |

### Architectural concerns

- **ReScript + TypeScript + React/Ink**: The dependency chain is heavy. The CLI
  components are written in ReScript (compiles to JS), the core library in
  TypeScript, and the interactive UI uses Ink (React for terminals). This
  requires Node.js and npm at runtime.
- **Sequential GitHub API calls**: PR existence checks and creation happen one
  at a time, no parallelism.
- **No configuration file**: All configuration is via environment variables or
  CLI flags.

### Notable open PRs on upstream

- [#17](https://github.com/keanemind/jj-stack/pull/17) — Fix non-user bookmark crash
- [#16](https://github.com/keanemind/jj-stack/pull/16) — Fork workflow detection with error message
- [#15](https://github.com/keanemind/jj-stack/pull/15) — Use change descriptions for PR body
- [#13](https://github.com/keanemind/jj-stack/pull/13) — GitHub Enterprise support
- [#2](https://github.com/keanemind/jj-stack/pull/2) — GitHub PR template support

## 6. Rust Ecosystem for Each Interface

### jj (Jujutsu) integration

**Approach**: Shell out to `jj` CLI, parse JSON output with serde.

The `jj` CLI is the stable interface. While `jj-lib` (v0.32, pre-1.0) exists
as a Rust crate, its API is still maturing and subject to breaking changes. The
CLI provides structured JSON output via `--template` and is the recommended
integration point for external tools.

Key `jj` commands used:
- `jj bookmark list` — list bookmarks with filtering
- `jj log` — commit history with custom templates, JSON output
- `jj git push` — push bookmarks to remote
- `jj git remote list` — list remotes

All of these support `--config 'ui.paginate=never'` and custom output
templates for machine-readable output.

### Git

**Not needed as a direct dependency.**

The original jj-stack called `git remote get-url origin` directly, which broke
in workspaces (no `.git` directory) and would also break in non-colocated repos
(no `.git` at all). The fork fixed remotes by using `jj git remote list`.

For jack, we take this further: **all git-related operations go through `jj`
commands**, never through `git` or git libraries directly. This means:

- `jj git remote list` — read remote URLs
- `jj git push` — push bookmarks
- `jj git fetch` — fetch from remotes

This approach makes workspaces and non-colocated repos work automatically,
because `jj` knows how to find its repo store regardless of the working
directory layout. No `git2`, `gix`, or `git` CLI dependency needed.

### GitHub API

**Crate**: `octocrab`

Modern, actively maintained GitHub API client for Rust. Covers all needed
operations:
- PR CRUD (create, read, update)
- Issue comments (for stack comments)
- User authentication validation
- Repository metadata

Alternatives considered:
- `octorust` — auto-generated from OpenAPI spec, always in sync but less
  ergonomic.
- `hubcaps` — older, less maintained.

### CLI framework

**Crate**: `clap` v4 with derive macros

The de facto standard. Provides subcommands, flags, help generation,
completions. No reason to consider alternatives.

### Terminal UI

**Crates**:
- `inquire` — interactive prompts and selection lists. Lightweight, focused.
  Perfect for bookmark selection.
- `indicatif` — progress bars and spinners. Thread-safe. Good for showing
  submission progress.
- `ratatui` — full TUI framework. Only needed if we want a rich interactive
  display later.

### Async runtime

**Crate**: `tokio`

Industry standard. Required by `octocrab` for HTTP calls. `async-std` is
discontinued as of early 2025.

### Error handling

**Crates**:
- `thiserror` — derive macros for defining error types.
- `anyhow` — flexible error propagation with `.context()`.
- `miette` — pretty diagnostic output for user-facing errors.

### JSON & serialization

**Crates**: `serde` + `serde_json`

Standard. Define Rust structs matching jj's JSON output templates and
GitHub API responses, deserialize automatically.

### Process execution

**Standard library**: `std::process::Command` for synchronous calls,
`tokio::process::Command` for async. No additional crate needed.

## 7. Workspaces and Non-Colocated Repos

This is a critical design consideration for jack. jj supports two repo layouts
that jj-stack cannot handle:

### jj Workspaces

Created with `jj workspace add <path>`. The new workspace directory contains
only a `.jj/` directory that points back to the main repo's store. There is no
`.git/` directory in the workspace. Any tool that calls `git` commands directly
will fail with `fatal: not a git repository`.

This is what causes [upstream issue #19](https://github.com/keanemind/jj-stack/issues/19).
The [fork](https://github.com/glennib/jj-stack) partially fixed this by
switching `git remote get-url` to `jj git remote list`, but any remaining
direct `git` usage would still break.

### Non-colocated repos

A jj repo can be initialized without colocating with git (`jj git clone` vs
`jj git clone --colocate`). In a non-colocated repo, jj manages an internal
git backend at `.jj/repo/store/git/` — there is no `.git/` directory at all.
Git commands, git libraries (git2, gix), and any tool that assumes a `.git/`
directory will not work.

However, `jj git remote list`, `jj git push`, `jj git fetch`, and all other
`jj git` subcommands work correctly because jj knows where its internal git
store is.

### Design principle for jack

**Never call `git` directly. Never use git libraries. Always go through `jj`.**

This single rule makes workspaces and non-colocated repos work automatically,
with zero special-case handling. It also eliminates `git2` (C dependency) or
`gix` from the dependency tree, simplifying builds.

## 8. Opportunities for Improvement

These are areas where the Rust rewrite can do better than jj-stack:

1. **Workspace and non-colocated support** — by using only `jj` commands, never
   `git` directly (see section 7).
2. **Parallel GitHub API calls** — check existing PRs and create new ones
   concurrently where dependencies allow.
3. **PR body from change descriptions** — use the full jj change description as
   the PR body, not just the first line as title.
4. **Draft PR support** — `--draft` flag to create PRs as drafts.
5. **Configurable default branch** — detect from `trunk()` remote bookmarks
   rather than hardcoding names.
6. **GitHub Enterprise** — configurable API base URL behind the forge trait.
7. **Fork workflow** — push to a fork remote, create PRs against upstream.
8. **Stack comments with PR titles** — more readable than bookmark names.
9. **`--help` anywhere** — help should work outside a jj repo.
10. **Graceful handling of non-user bookmarks** — filter cleanly, don't crash.
11. **Forge trait abstraction** — GitHub first, but designed so
    Forgejo/GitLab/etc. can be added later.
12. **Single static binary** — no runtime dependencies (Node.js, npm).
13. **Config file** — `.jack.toml` or similar for per-repo settings.
14. **Structured error messages** — using miette for clear diagnostics.

## 9. Comparison: jj-stack vs jack

| Dimension | jj-stack (current) | jack (goal) |
|---|---|---|
| Language | TypeScript + ReScript | Rust (single binary) |
| Distribution | npm install | Static binary (cargo-binstall, GitHub releases) |
| Runtime deps | Node.js, npm | None |
| jj interface | Shell out, custom JSON templates | Shell out, serde structs |
| Git dependency | Called `git` directly (broke workspaces) | None — all git ops via `jj` |
| GitHub API | Octokit (JS) | octocrab, behind forge trait |
| Forge support | GitHub only | GitHub first, trait for future forges |
| Auth | gh CLI / env vars | gh CLI / env vars (same) |
| PR creation | Title only, no body | Full description from change descriptions |
| Draft PRs | Not supported | Supported (`--draft`) |
| Default branch | Hardcoded main/master/trunk | Auto-detect from trunk() |
| GitHub Enterprise | Not supported | Supported (configurable base URL) |
| Fork workflow | Not supported | Supported |
| Workspace support | Broken ([#19][i19]) | Works (no `git` dependency) |
| Non-colocated repos | Broken (requires `.git`) | Works (no `git` dependency) |
| Stack comments | Bookmark names | PR titles |
| PR base updates | Sequential | Parallel where possible |
| Interactive UI | React/Ink | inquire (lightweight) |
| Error messages | Basic console output | Structured diagnostics (miette) |
| Config file | None | `.jack.toml` or similar |
| Non-user bookmarks | Crashes | Handled gracefully |
| `--help` | Requires jj repo | Works anywhere |
| Merge commits | Excluded (correct) | Excluded (same) |
| Idempotency | Yes | Yes |
| Multiple bookmarks/change | Supported | Supported |
| Performance | Node.js startup overhead | Near-instant startup |

[i19]: https://github.com/keanemind/jj-stack/issues/19
