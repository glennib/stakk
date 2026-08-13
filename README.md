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
- **No bookmarks required** — stakk discovers unbookmarked heads and creates bookmarks for them,
  interactively or from explicit flags.
- **Interactive TUI** — `stakk` opens a graph of every branch stack,
  then a screen for assigning bookmarks to the commits that need them.
  Each commit cycles through: `[x]` existing → `[~]` auto → `[>]` typed by hand → `[+]` generated `stakk-xxxx` → `[*]`
  custom command → `[ ]` skip.
- **Auto bookmark naming** — the `[~]auto` state derives names from commit descriptions and file paths via TF-IDF
  scoring; `r` cycles alternatives and `--auto-prefix` brands them (e.g. `gb-caching-database`).
- **Stacked PR submission** — creates or updates GitHub PRs with correct base branches so each PR shows only its own
  diff; `--pr-mode draft` creates new ones as drafts.
- **Stack-awareness comments** — every PR gets the full stack with links,
  updated in place on re-runs and rendered with customizable [minijinja](https://github.com/mitsuhiko/minijinja)
  templates.
  See [Stack info placement](#stack-info-placement).
- **Idempotent** — re-running `stakk submit` is always safe.
  Existing PRs are updated, never duplicated.
- **Dry-run mode** — `--dry-run` shows exactly what would happen without
  touching GitHub — or the repo: no bookmarks are created, nothing is pushed.
- **Non-interactive selection** — `--keep`/`--new`/`--new-auto`/`--new-command` build the exact same
  submission the TUI would, without a terminal.
- **PR titles and bodies from descriptions** — populated from jj change descriptions on creation, so manual edits on
  GitHub survive; `--sync-pr-content` opts into keeping them in sync.
- **Self-documenting** — `stakk docs` prints the bundled documentation, version-locked to the binary you are running.
- **No direct git usage** — all VCS operations go through `jj` commands, so
  workspaces and non-colocated repos work automatically.
- **Forge-agnostic core** — GitHub is the first implementation, but the
  submission logic is decoupled behind a `Forge` trait.

## Installation

### Requirements

stakk shells out to the [`jj`](https://github.com/jj-vcs/jj) CLI, which must be installed and on your `PATH`.
The minimum supported jj version is **0.39.0**.
Older versions may work but are untested; stakk prints a warning when it detects one.
Raising that floor is not a breaking change — see [Stability](#stability).

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
# Submit interactively — pick a stack and assign bookmarks in the TUI
stakk

# See your stacks without submitting (offline: jj only, never GitHub)
stakk show

# Preview a submission without touching the repo or GitHub
stakk submit --keep feat-auth --keep feat-api --keep my-feature --dry-run

# Submit without the TUI — every --keep is one PR boundary
stakk submit --keep feat-auth --keep feat-api --keep my-feature
```

## How stacking works

In jj, bookmarks point at changes.
When bookmarks form a linear chain — each building on the previous — they represent a stack.
Create the bookmarks yourself, or let stakk discover unbookmarked heads and create them for you:

```text
 ○  feat-ui    ← leaf
 ○  feat-api
 ○  feat-auth
 ◆  main       ← trunk
```

Picking `feat-ui` as the tip and keeping all three bookmarks — in the TUI,
or as `stakk submit --keep feat-auth --keep feat-api --keep feat-ui` —
pushes every one of them and creates or updates one PR per bookmark, basing each on the bookmark below it:

- `feat-auth` → PR targeting `main`
- `feat-api` → PR targeting `feat-auth`
- `feat-ui` → PR targeting `feat-api`

Each PR shows only its own diff, and a stack comment on every PR links all related PRs together.
The comment draws the stack the same way `stakk show` and the TUI do — leaf at the top, trunk at the bottom —
and marks the PR you are looking at:

```text
Stack of 3 PRs — merges into main

○ #14 feat-ui
● #13 feat-api ← this PR
○ #12 feat-auth
◆ main
```

Re-running `stakk submit` is always safe — it updates existing PRs rather than creating duplicates.

## Stack info placement

`--stack-placement` decides where the stack overview lives on each PR: a separate PR `comment` (default),
a fenced section in the PR `body`, `none` (write nothing and remove what is already there), or `ignore`
(write nothing and touch nothing).
Switching between `comment` and `body` migrates automatically,
and a submission that produces a single PR is not a stack, so no stack info is written.

Full mode table, migration details, and stack comment templating: [docs/template.md](docs/template.md),
or run `stakk docs template`.

## Configuration

Settings come from CLI flags, then `STAKK_`-prefixed environment variables, then a repository `stakk.toml`
(found by walking up to the jj workspace root),
then the user config (`~/.config/stakk/config.toml` on Linux), then built-in defaults.
All fields are optional, and unknown fields cause a parse error.

```toml
# stakk.toml
remote = "origin"
pr_mode = "draft"
stack_placement = "body"
auto_prefix = "gb-"
```

Every config key and environment variable, the `inherit` field, and worked examples: [docs/config.md](docs/config.md),
or run `stakk docs config`.

### GitHub Enterprise Server

github.com works out of the box.
For a GitHub Enterprise Server host, name the host with `--github-host`, `STAKK_GITHUB_HOST`,
`github_host` in `stakk.toml`, or `GH_HOST` —
stakk then accepts remotes on that host and uses its API at `https://<host>/api/v3`.
Tokens are resolved per host, mirroring the GitHub CLI, so an Enterprise token is never sent to github.com.

Host resolution order and the API base: [docs/config.md](docs/config.md), or run `stakk docs config`.
The `gh` commands that set the host up, the per-host token order, what to do when it fails, and how to check the setup:
[docs/auth.md](docs/auth.md), or run `stakk docs auth`.

## Usage

`stakk --help` and `stakk <subcommand> --help` are the flag reference: every flag, its default,
and its environment variable.

### `stakk`

Launches the interactive submission flow.
A ratatui TUI shows a graph of all branch stacks; select a leaf branch, then toggle bookmarks on commits that need them.
Works even in repos with no pre-existing bookmarks — stakk creates `stakk-<change_id>` bookmarks for unmarked commits.
Identical to `stakk submit` with no arguments.

### `stakk submit`

Submit a stack of bookmarks as stacked PRs.
With no selection flags it runs the interactive flow above — `stakk` and `stakk submit` are the same thing.
The selection flags below replace the TUI with an explicit, scriptable selection.

#### Non-interactive selection

`--keep`, `--new`, `--new-auto` and `--new-command` replace the TUI with a fully explicit, scriptable selection:
every PR boundary is named on the command line, all marks must lie on one trunk-to-tip path,
the topmost mark is the tip, and bookmarks on the path that are not kept fold into the PR above them.
`rev` is a change id or commit id prefix as printed by `stakk show`, which makes submission a two-command loop:

```console
stakk show --format=json
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk --dry-run
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk
```

Every selection rule, the machine-readable `stakk::selection::*` diagnostic codes, and the JSON schema:
[docs/scripting.md](docs/scripting.md), or run `stakk docs scripting`.
Pointing a coding agent at `stakk docs scripting` is the fastest way to bring it up to speed.

#### PR titles and bodies

PR titles come from the first line of the jj change description and bodies from everything after it;
segments with multiple commits join their descriptions with `---` separators.
Both are written only on PR creation, so manually edited PR descriptions are never overwritten — `--sync-pr-content`
(`title`, `body`, `all`) opts into updating existing PRs, and only changed fields are sent.

Descriptions are reflowed on the way into the PR body: hard-wrapped prose lines are joined into soft-wrapped paragraphs,
so a commit message wrapped at 72 columns does not render as ragged lines on GitHub.
Structural Markdown — headers, lists, tables, block quotes, fenced and indented code, thematic breaks —
is passed through verbatim.
`--trailers strip` removes the trailing key/value block
(`Signed-off-by`, `Co-authored-by`, `Refs`, …) from the generated body; the default `keep` passes it through.

#### Custom bookmark names

`--bookmark-command` names bookmarks with an external program.
The command is run through `sh -c` (Unix) or `cmd /C` (Windows),
receives a JSON description of one segment of commits on stdin, and must print a single bookmark name on stdout.
It powers the `[*]` state in the TUI and the `--new-command` selection flag.
The full JSON schema, with a worked example, is in `stakk submit --help`;
the surrounding context is in [docs/template.md](docs/template.md), or run `stakk docs template`.

#### Immutable commits

Commits that jj considers immutable cannot get a new bookmark: the default bookmarks revset excludes `immutable()`,
so stakk would create a PR it could never see again on the next run.
The TUI locks such rows to `[ ]` and explains why, `--new <rev>` on one fails with `stakk::selection::rev_immutable`,
and the commits are annotated in `stakk show`.
A row only unlocks when the commit is a real segment boundary —
a bookmark exists on it *and* the bookmarks revset does not filter that bookmark out
(the default `~ immutable()` term does).
So if you really need a PR there, create the bookmark yourself and drop `~ immutable()` from `--bookmarks-revset`;
otherwise move the work onto mutable commits.
The non-interactive side of this is in [docs/scripting.md](docs/scripting.md), or run `stakk docs scripting`.

### `stakk show`

Display repository status and all bookmark stacks without submitting.
Fully offline: only `jj` is queried, never GitHub — PR state is `stakk submit --dry-run`'s job.
`show` and `submit` build the graph the same way, so `--bookmarks-revset`/`--heads-revset` apply to both:
what `show` prints is what `submit` would work on.

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

Bookmarks whose history contains a merge commit cannot be stacked and are left out of the graph; when that happens,
`pretty` prints a `(N bookmark(s) excluded due to merge commits)` footer.

`--format=json` emits a schema-versioned document for machine consumption
(scripts, agents); its change id prefixes and bookmark names can be passed directly to `stakk submit`.
It is a sparse projection — identifiers, commit titles, bookmarks and stack position, and nothing else —
while `--format=json-full` adds each commit's `commit_id`, full `description`, `author` and `files[]`.
Sparse is a strict subset of full: same schema, same field names, same values.
Field-by-field schema: [docs/scripting.md](docs/scripting.md), or run `stakk docs scripting`.

### `stakk docs [topic]`

Print the documentation bundled into the binary.
Because it ships inside the binary, it always describes the version you are actually running.
Run `stakk docs` with no arguments for the list of topics.
Each topic is one of the Markdown files in [docs/](docs/), which is also where to read them on GitHub.

At a terminal the prose is re-flowed to your terminal width.
Redirected, the source is emitted verbatim —
so `stakk docs scripting >> AGENTS.md` writes exactly the Markdown in `docs/scripting.md`,
which makes it a one-line way to give a coding agent the full non-interactive workflow.

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

## Stability

stakk follows semantic versioning.
What that guarantee covers — the surface a script, an agent, or another tool may rely on:

**Stable.**
Changing these needs a major release.

- Subcommand names and their flags.
- `STAKK_`-prefixed environment variables.
- Config file keys and their defaults.
- The `stakk show` JSON document, under its `schema_version`
  (currently `2`; both the sparse `json` and the `json-full` projection report it).
- The JSON handed to `--bookmark-command` on stdin, under its own `schema_version` (currently `1`).
- Diagnostic codes (`stakk::…`).
- Exit codes.

**Not stable.** These may change in any release.

- The rendered text of `stakk docs` and `--help`.
  Doc topics may be added, reworded, or restructured at any time;
  only the `stakk docs <topic>` invocation shape is stable.
- The `pretty` output of `stakk show`.
- The TUI layout and keybindings.
- Spinner and progress text.
- Error message wording — the diagnostic codes are the contract, not the prose.

**Not a breaking change:** raising the minimum supported jj version.
The check is warn-only — stakk runs against an older jj and says so — so the floor can move in any release,
normally the one that actually adopts a newer jj's behaviour.

## License

MIT OR Apache-2.0
