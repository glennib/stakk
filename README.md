# stakk

**stakk** bridges [Jujutsu](https://github.com/jj-vcs/jj) bookmarks to GitHub stacked pull requests.

It is not a jj wrapper.
It complements jj by reading your local bookmark state and turning it into a coherent set of GitHub PRs
that merge into each other in the correct order — with stack-awareness comments, correct base branches,
and idempotent updates.

![Interactive stakk submission flow](media/stakk.gif)

## Features

- **Automatic stack detection** — analyzes the jj change graph to find bookmark
  chains and their topological order.
- **No bookmarks required** — stakk discovers unbookmarked heads
  and lets you create bookmarks on-the-fly via the interactive TUI.
  Auto-generated `stakk-<change_id>` names keep things simple.
- **Auto bookmark naming** — the `[~]auto` toggle in the TUI generates descriptive bookmark names from commit
  descriptions and file paths using TF-IDF (term frequency–inverse document frequency) scoring.
  Press `r` to cycle through alternative names.
  An optional `--auto-prefix` lets you brand the names (e.g. `gb-caching-database`).
- **Stacked PR submission** — creates or updates GitHub PRs with correct base
  branches so each PR shows only its own diff.
- **Stack-awareness comments** — adds a comment to every PR listing the full stack with links,
  updated in place on re-runs.
  Optionally, the stack info can be placed in the PR body instead (`--stack-placement body`) or disabled entirely
  (`--stack-placement none`, which also removes existing stack comments and body fences).
  Comments are rendered with [minijinja](https://github.com/mitsuhiko/minijinja) templates
  and can be customized with `--template` or the `STAKK_TEMPLATE` environment variable.
- **Idempotent** — re-running `stakk submit` is always safe.
  Existing PRs are updated, never duplicated.
- **Dry-run mode** — `--dry-run` shows exactly what would happen without
  touching GitHub.
- **Interactive TUI** — running `stakk` without arguments launches a ratatui TUI: a graph view shows all branch stacks,
  then a bookmark assignment screen lets you toggle bookmarks on unmarked commits before submitting.
  Each commit cycles through: `[x]` existing → `[~]` auto → `[+]` generated `stakk-xxxx` → `[*]` custom command → `[ ]`
  skip.
- **Draft PRs** — `--draft` creates new PRs as drafts.
- **PR body from descriptions** — PR titles and bodies are populated from jj change descriptions.
  Manually edited PR bodies are never overwritten.
- **No direct git usage** — all VCS operations go through `jj` commands, so
  workspaces and non-colocated repos work automatically.
- **Forge-agnostic core** — GitHub is the first implementation, but the
  submission logic is decoupled behind a `Forge` trait.

## Origins

stakk is inspired by [jj-stack](https://github.com/keanemind/jj-stack),
a TypeScript/ReScript CLI that does the same job. jj-stack's core algorithms — change graph construction,
segment grouping, topological ordering — directly informed stakk's design.

stakk reimplements these ideas in Rust to continue the development and to address new features and desired changes.

## Installation

### Requirements

stakk shells out to the [`jj`](https://github.com/jj-vcs/jj) CLI, which must be installed and on your `PATH`.
The minimum supported jj version is **0.39.0**.
Older versions may work but are untested; stakk prints a warning when it detects one.

### mise (recommended)

```text
mise use -g 'github:glennib/stakk'
```

Or from crates.io:

```text
mise use -g 'cargo:stakk'
```

### cargo-binstall

```text
cargo binstall stakk
```

### cargo install

```text
cargo install stakk
```

### Pre-built binaries

Download from the [latest release](https://github.com/glennib/stakk/releases/latest).

## Quick start

```text
# Submit interactively — pick a stack and assign bookmarks via TUI
stakk

# Works even without any bookmarks — the TUI lets you create them
stakk

# Submit a specific bookmark (and its ancestors) as stacked PRs
stakk submit my-feature

# Preview what would happen without doing anything
stakk submit my-feature --dry-run

# Create PRs as drafts
stakk submit my-feature --draft

# See your bookmark stacks without submitting
stakk show
```

## How stacking works

In jj, bookmarks point at changes.
When bookmarks form a linear chain — each building on the previous — they represent a stack.
You can create bookmarks yourself, or let stakk discover unbookmarked heads and create them interactively:

```text
trunk
  └── feat-auth        ← bookmark 1
        └── feat-api   ← bookmark 2
              └── feat-ui  ← bookmark 3
```

When you run `stakk submit feat-ui`, stakk:

1. **Analyzes** the change graph to find the stack containing `feat-ui` and
   all its ancestors (`feat-auth`, `feat-api`, `feat-ui`).
2. **Plans** the submission by checking GitHub for existing PRs, determining
   which bookmarks need pushing, which PRs need creating, and which base
   branches need updating.
3. **Executes** the plan: pushes bookmarks, creates or updates PRs with
   correct base branches, and adds stack-awareness comments to every PR.

The result on GitHub:

- `feat-auth` → PR targeting `main`
- `feat-api` → PR targeting `feat-auth`
- `feat-ui` → PR targeting `feat-api`

Each PR shows only its own diff, and a stack comment on every PR links all related PRs together:

![Stack comment example on a GitHub PR](media/pr-comment.png)

Re-running `stakk submit` is always safe — it updates existing PRs rather than creating duplicates.

## Configuration

stakk loads settings from TOML config files, environment variables, and CLI flags.
The full precedence order (highest to lowest):

1. **CLI flags** — `--remote`, `--draft`, `--pr-mode`, etc.
2. **Environment variables** — `STAKK_REMOTE`, `STAKK_DRAFT`, etc.
3. **Repository config** — `stakk.toml` found by walking up from the current
   directory
4. **User config** — `~/.config/stakk/config.toml` (Linux),
   `~/Library/Application Support/stakk/config.toml` (macOS),
   `%APPDATA%\stakk\config\config.toml` (Windows)
5. **Built-in defaults**

### Config files

stakk discovers config files automatically — no flags needed.

**Repository config** is found by walking from the current directory toward the jj workspace root
(the directory containing `.jj/`), stopping at the first `stakk.toml` found.
The search does not continue past the repo root.
To share config across multiple repos, use `--config` or the `STAKK_CONFIG` environment variable.

**User config** is loaded from your platform's standard config directory.
On Linux this is typically `~/.config/stakk/config.toml`.

Both files use the same format.
When both exist, settings from the repo config take precedence —
the user config fills in any fields the repo config leaves unset.

### Config file format

All fields are optional.
Absent fields fall back to the next level in the precedence chain.

```toml
# stakk.toml — example with all available fields

# Git remote to push to (default: "origin")
remote = "origin"

# PR creation mode: "regular" or "draft" (default: "regular")
pr_mode = "draft"

# Path to a custom minijinja template for stack comments
template = "/path/to/my-template.md.jinja"

# Where to place stack info: "comment", "body", or "none" (default: "comment")
# "none" writes no stack info and removes existing stack comments/body fences
# on the next submit (e.g. if you rely on GitHub's native stacked PRs).
stack_placement = "body"

# Prefix for auto-generated bookmark names (default: none)
auto_prefix = "gb-"

# Revset for discovering bookmarks (default: "mine() ~ trunk() ~ immutable()")
# Note: the default excludes bookmarks on immutable commits. Stale untracked
# remote bookmarks can pin commits immutable — clean them up with
# `jj bookmark forget --include-remotes 'glob:<pattern>'`, or drop the
# `~ immutable()` term for one run to include them.
bookmarks_revset = "mine() ~ trunk() ~ immutable()"

# Revset for discovering unbookmarked heads
# (default: "heads((mine() ~ empty() ~ immutable()) & trunk()..)")
heads_revset = "heads((mine() ~ empty() ~ immutable()) & trunk()..)"

# Sync PR title/body from commits on every submit (default: "none")
# Options: "none", "title", "body", "all"
sync_pr_content = "all"

# How to handle git commit trailers in PR bodies (default: "keep")
# Options: "keep", "strip"
trailers = "strip"

# Shell command for generating custom bookmark names
bookmark_command = "my-bookmark-namer"

# Whether to merge with the user config (default: true)
# Set to false in a repo config to ignore the user config entirely.
inherit = true
```

Unknown fields cause a parse error, so typos are caught early.

### The `inherit` field

By default, repo config and user config are merged: the repo config wins for any field it sets,
and the user config fills in the rest.
If a repo needs to ignore the user config entirely
(e.g. to enforce team-wide settings), set `inherit = false` in the repo-level `stakk.toml`:

```toml
# stakk.toml — standalone, ignores user config
inherit = false
pr_mode = "regular"
stack_placement = "comment"
```

`inherit` only has meaning in a repo config.
It is not merged from the user config.

### Examples

**User config** — personal defaults across all repos:

```toml
# ~/.config/stakk/config.toml
pr_mode = "draft"
stack_placement = "body"
```

**Repo config** — override remote for this repo, inherit everything else:

```toml
# stakk.toml (in repo root)
remote = "upstream"
```

With both files above, running `stakk submit my-feature` uses `remote = "upstream"` from the repo config
and `pr_mode = "draft"`, `stack_placement = "body"` from the user config.
Passing `--pr-mode regular` on the command line overrides all of them.

**Team-enforced config** — no user config inheritance:

```toml
# stakk.toml (in repo root)
inherit = false
remote = "origin"
pr_mode = "regular"
stack_placement = "comment"
```

## Environment variables

| Variable | Description |
|----------|-------------|
| `STAKK_CONFIG` | Path to config file, overrides automatic discovery (overridden by `--config`) |
| `STAKK_REMOTE` | Default git remote to push to (overridden by `--remote`) |
| `STAKK_PR_MODE` | PR creation mode: `regular` or `draft` (overridden by `--pr-mode`) |
| `STAKK_DRAFT` | Set to `true` to always create draft PRs (overridden by `--draft`) |
| `STAKK_TEMPLATE` | Path to a custom minijinja template for stack comments (overridden by `--template`) |
| `STAKK_STACK_PLACEMENT` | Where to place the stack info: `comment` (default), `body`, or `none` (overridden by `--stack-placement`) |
| `STAKK_AUTO_PREFIX` | Prefix for auto-generated bookmark names (overridden by `--auto-prefix`) |
| `STAKK_SYNC_PR_CONTENT` | Sync PR title/body from commits: `none` (default), `title`, `body`, or `all` (overridden by `--sync-pr-content`) |
| `STAKK_TRAILERS` | Whether to keep or strip git commit trailers in PR bodies: `keep` (default) or `strip` (overridden by `--trailers`) |
| `STAKK_BOOKMARK_COMMAND` | Shell command for generating custom bookmark names (overridden by `--bookmark-command`) |
| `GITHUB_TOKEN` | GitHub personal access token (see `stakk auth setup`) |
| `GH_TOKEN` | Alternative to `GITHUB_TOKEN` |

CLI flags take precedence over environment variables, which take precedence over config files.
See [Configuration](#configuration) for the full precedence order.

## Usage

### `stakk` (no arguments)

Launches the interactive submission flow.
A ratatui TUI shows a graph of all branch stacks; select a leaf branch, then toggle bookmarks on commits that need them.
Works even in repos with no pre-existing bookmarks — stakk creates `stakk-<change_id>` bookmarks for unmarked commits.
Equivalent to `stakk submit` without arguments.

### `stakk submit [bookmark]`

Submit a bookmark and all its ancestors as stacked PRs.
When run without a bookmark argument, an interactive ratatui TUI lets you select a branch from a graph view,
then assign bookmarks to any unmarked commits before submitting.

| Flag | Env var | Description |
|------|--------|-------------|
| `--dry-run` | | Show the submission plan without executing |
| `--keep <bookmark>` | | Non-interactive: keep an existing bookmark as a PR boundary (repeatable) |
| `--keep-all` | | Non-interactive: keep every existing bookmark on the selected path |
| `--new <rev>[=<name>]` | | Non-interactive: new bookmark at `rev` — `stakk-<change_id>` by default, or `name` (repeatable) |
| `--new-auto <rev>` | | Non-interactive: new TF-IDF-named bookmark at `rev`, honoring `--auto-prefix`; falls back to `stakk-<change_id>` when nothing can be derived or the name is taken (repeatable) |
| `--new-command <rev>` | | Non-interactive: new bookmark at `rev` named by `--bookmark-command` (repeatable) |
| `--draft` | `STAKK_DRAFT` | Create new PRs as drafts |
| `--remote <name>` | `STAKK_REMOTE` | Push to a specific remote (default: `origin`) |
| `--template <path>` | `STAKK_TEMPLATE` | Use a custom minijinja template for stack comments |
| `--stack-placement <mode>` | `STAKK_STACK_PLACEMENT` | Place stack info as a PR `comment` (default), in the PR `body`, or write none at all with `none` |
| `--auto-prefix <prefix>` | `STAKK_AUTO_PREFIX` | Prefix for `[~]auto` bookmark names (e.g. `gb-`) |
| `--sync-pr-content <mode>` | `STAKK_SYNC_PR_CONTENT` | Sync PR title/body from commits: `none` (default), `title`, `body`, `all` |
| `--trailers <mode>` | `STAKK_TRAILERS` | Keep or strip git commit trailers in PR bodies: `keep` (default), `strip` |

The selection flags (`--keep`, `--keep-all`, `--new`, `--new-auto`, `--new-command`) replace the TUI with a fully
explicit, scriptable selection; they conflict with the positional bookmark argument and are deliberately CLI-only (no
env vars, no config keys — like `--dry-run`, per-invocation decisions make surprising defaults).
The marks fully determine the PR set: nothing is implicit.
All marks must lie on one trunk-to-tip path, the topmost mark is the tip of the submission,
and bookmarks on the path that are not kept fold into the PR above them.
`rev` is a change id or commit id prefix as printed by `stakk show`.
Bare `--keep-all` requires the choice of stack to be unambiguous — a single stack,
or several that agree on their bookmarks
(e.g. differing only in unbookmarked heads such as the working copy); anchor it with `--keep`/`--new` otherwise.
Commits already carrying an explicit mark are skipped by `--keep-all` (explicit beats bulk).

#### Agent / scripting usage

Non-interactive submission is a two-command loop — discover, then submit.
Everything `stakk submit` needs (change id prefixes, bookmark names) is in `stakk show`'s output:

```console
stakk show --format=json | jq '.stacks[0].segments[].commits[] | {short_change_id, description, local_bookmark_names}'
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk --dry-run
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk
```

`--dry-run` is fully inert: it prints the planned bookmark creations and PR actions without creating, pushing,
or changing anything — safe for validation before the real run.
Selection errors carry machine-readable diagnostic codes (`stakk::selection::*`) and point back at `stakk show`.

PR titles come from the first line of the jj change description.
PR bodies are populated from the full description (everything after the title line).
For segments with multiple commits, descriptions are joined with `---` separators.
By default, titles and bodies are only set on PR creation — manually edited PR descriptions are never overwritten.
Use `--sync-pr-content` to update existing PRs: `title` syncs only the title, `body` only the body, or `all` for both.
Only fields that actually changed are updated.

### `stakk show`

Display repository status and all bookmark stacks without submitting.
Fully offline: only `jj` is queried, never GitHub — PR state is `stakk submit --dry-run`'s job.

| Flag | Env var | Description |
|------|--------|-------------|
| `--format <format>` | | Output format: `pretty` (default) or `json` |

The default `pretty` format renders a jj-log-style graph of all stacks, always fully expanded.
Every commit row carries its short change id, bookmarks, and description summary;
immutable commits and bookmarks excluded by the bookmarks revset are annotated:

```text
Default branch: main
Remote: origin git@github.com:you/repo.git (you/repo)

 ○  wmtk  feat-a  (no description set)
 ○  wmtl  "feat a work"
 │ ○  rlkv  feat-b  "feat b work"  (immutable — bookmark old-mark excluded by --bookmarks-revset)
 ├─╯
 ○  qzvs  base  "extend base"
 ○  qzvt  "add base"
 ◆  trunk
```

`--format=json` emits a schema-versioned JSON document for machine consumption (scripts, agents).
Identifiers in the document — change id prefixes and bookmark names — can be passed directly to `stakk submit`.

- `schema_version` — currently `1`; bumped on breaking schema changes
- `default_branch`
- `remotes[]` — `name`, `url`, `github` (`owner/repo`, or `null` for
  non-GitHub remotes)
- `excluded_bookmark_count` — bookmarks excluded due to merge commits
- `stacks[]` — one per leaf, trunk-to-leaf; shared ancestor segments are
  repeated in every stack that contains them
  - `segments[]` — `bookmark_names[]` and `commits[]` (oldest first)
    - each commit: `change_id`, `short_change_id`, `commit_id`,
      `description` (full), `author` (`name`, `email`, `timestamp`),
      `files[]`, `is_immutable`, `local_bookmark_names[]` (unfiltered —
      includes bookmarks the bookmarks revset excluded), `is_boundary`
      (the commit its segment's bookmarks point at), `is_leaf` (the tip
      of its stack)

### `stakk completions <shell>`

Generate shell completions.
Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

```text
# Zsh — add to your fpath
stakk completions zsh > ~/.zfunc/_stakk

# Bash
stakk completions bash > ~/.local/share/bash-completion/completions/stakk

# Fish
stakk completions fish > ~/.config/fish/completions/stakk.fish
```

### `stakk auth test`

Validate that GitHub authentication is working and print the authenticated username.

### `stakk auth setup`

Print instructions for setting up authentication. stakk resolves a GitHub token in this order:

1. **GitHub CLI** (`gh auth token`) — recommended
2. **`GITHUB_TOKEN`** environment variable
3. **`GH_TOKEN`** environment variable

## Design

stakk never calls `git` directly.
All git operations go through `jj` subcommands (`jj git push`, `jj git remote list`, etc.).
This means stakk works automatically in jj workspaces and non-colocated repositories —
two cases where calling `git` directly fails.

All forge interaction goes through a `Forge` trait.
GitHub is the first (and currently only) implementation, but the core submission logic is forge-agnostic.
This opens the door to Forgejo, GitLab, or other platforms in the future.

The submission pipeline is split into three phases:

- **Analyze** — pure function, no I/O, fully testable with mock data
- **Plan** — queries the forge for existing PRs, determines actions
- **Execute** — pushes bookmarks, creates/updates PRs, manages comments

This separation makes the business logic testable without hitting real APIs, and `--dry-run` falls out naturally
(run phases 1 and 2, skip 3).

## License

MIT OR Apache-2.0
