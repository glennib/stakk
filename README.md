# stakk

**stakk** bridges [Jujutsu](https://github.com/jj-vcs/jj) bookmarks to GitHub stacked pull requests.
It reads your change graph, lets you select a stack and name bookmarks for the commits that need them,
then pushes and maintains one PR per bookmark: correct base branches, stack comments, no duplicates on re-runs.

It is not a jj wrapper. jj stays in charge of your commits and bookmarks;
stakk takes over only where that local state has to exist on GitHub.

![Interactive stakk submission flow](media/stakk.gif)

## Features

- **Automatic stack detection** — finds bookmark chains and their topological order in the jj change graph,
  including unbookmarked heads, which stakk can bookmark for you.
- **Interactive TUI** — a graph of every branch stack, then a screen for assigning bookmarks.
  Each commit cycles through: `[x]` existing → `[~]` auto → `[>]` typed by hand → `[+]` generated `stakk-xxxx` → `[*]`
  custom command → `[ ]` skip.
- **Auto bookmark naming** — the `[~]auto` state derives names from commit descriptions and file paths via TF-IDF
  scoring; `r` cycles alternatives and `--auto-prefix` brands them (e.g. `gb-caching-database`).
- **Non-interactive selection** — `--keep`/`--new`/`--new-auto`/`--new-command` build the exact same
  submission the TUI would, without a terminal.
- **Stack-awareness comments** — every PR gets the full stack with links,
  updated in place on re-runs and rendered with customizable [minijinja](https://github.com/mitsuhiko/minijinja)
  templates.
  See [Stack info placement](#stack-info-placement).
- **Dry-run mode** — `--dry-run` prints the submission plan and stops: no bookmark is created,
  nothing is pushed, and nothing is written to GitHub.
- **No direct git usage** — all VCS operations go through `jj` commands, so
  workspaces and non-colocated repos work automatically.

## Installation

### Requirements

stakk shells out to the [`jj`](https://github.com/jj-vcs/jj) CLI, which must be installed and on your `PATH`.
The minimum supported jj version is **0.39.0**.
Older versions may work but are untested; stakk prints a warning when it detects one.
Raising that floor is not a breaking change — see [Stability](#stability).

### mise (recommended)

```shell
mise use -g 'github:glennib/stakk'
```

### Other methods

```shell
mise use -g 'cargo:stakk' # from crates.io
cargo binstall stakk # using cargo-binstall
cargo install stakk # install from source
```

Or pre-built binaries from the [latest release](https://github.com/glennib/stakk/releases/latest).

## Quick start

```shell
# Submit interactively — pick a stack and assign bookmarks in the TUI
stakk

# See your stacks, and the change ids to name on the command line
# (offline: jj only, never GitHub)
stakk graph

# Submit without the TUI — one mark per PR boundary: keep an existing
# bookmark, name a new one at change qzvs, auto-name one at wmtk
stakk submit --keep feat-auth --new qzvs=feat-api --new-auto wmtk

# Add --dry-run to any submission to see the plan and stop
stakk submit --keep feat-auth --new qzvs=feat-api --new-auto wmtk --dry-run
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
The comment draws the stack the same way `stakk graph` and the TUI do — leaf at the top, trunk at the bottom —
and marks the PR you are looking at:

```text
Stack of 3 PRs — merges into main

○ #14 feat-ui
● #13 feat-api ← this PR
○ #12 feat-auth
◆ main
```

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

Every config key and environment variable, the `inherit` field, worked examples,
and what a repo-supplied `stakk.toml` is able to do: [docs/config.md](docs/config.md), or run `stakk docs config`.

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

`submit`, `graph` and `docs` each answer to their initial letter, so `stakk s`, `stakk g` and `stakk d` work too.
`completions` has no short form.

### `stakk`, `stakk submit`

Submit a stack of bookmarks as stacked PRs.
The two spellings are one command: with no selection flags, both launch the interactive flow —
a TUI graph of all branch stacks where you pick a leaf,
then a screen for toggling bookmarks onto the commits that need them.
Repos with no pre-existing bookmarks work too; unmarked commits get `stakk-<change_id>`.
The selection flags below replace the TUI with an explicit, scriptable selection.

#### Non-interactive selection

`--keep`, `--new`, `--new-auto` and `--new-command` replace the TUI with a fully explicit, scriptable selection:
every PR boundary is named on the command line, all marks must lie on one trunk-to-tip path,
the topmost mark is the tip, unmarked commits below it fold into the PR above them, and anything above it —
an unbookmarked work-in-progress head, typically — is not submitted at all.
`rev` is a change id or commit id prefix as printed by `stakk graph`, which makes submission a two-command loop:

```console
stakk graph --format=json
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk
```

Every selection rule and the machine-readable `stakk::selection::*` diagnostic codes: [docs/agents.md](docs/agents.md),
or run `stakk docs agents`.
Pointing a coding agent at `stakk docs agents` is the fastest way to bring it up to speed.
For a program rather than an agent, [docs/scripting.md](docs/scripting.md)
(`stakk docs scripting`) adds exit codes and a worked Python example.

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
and the commits are annotated in `stakk graph`.
Move the work onto mutable commits — or, if you really need a PR there,
create the bookmark yourself and drop `~ immutable()` from `--bookmarks-revset`.
Details, including the non-interactive side: [docs/agents.md](docs/agents.md), or run `stakk docs agents`.

### `stakk graph`

Display repository status and all bookmark stacks without submitting.
Fully offline: only `jj` is queried, never GitHub — PR state is `stakk submit --dry-run`'s job.
`stakk show` is an alias for the same command, deprecated and due for removal in a future major release.
`graph` and `submit` build the graph the same way, so `--bookmarks-revset`/`--heads-revset` apply to both:
what `graph` prints is what `submit` would work on.

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
`pretty` names them in a footer, and the JSON reports them in `excluded_bookmarks`.

`--format=json` emits a schema-versioned document for machine consumption
(scripts, agents); its change id prefixes and bookmark names can be passed directly to `stakk submit`.
It is a sparse projection — identifiers, commit titles, bookmarks and stack position —
and `--format=json-full` is a strict superset that adds each commit's `commit_id`, full `description`,
`author` and `files[]`.
Bookmarks carry their push state (`unpushed`, `diverged`, `synced`), derived from `jj` alone,
so a consumer can tell new work from an update without a network round trip.
Field-by-field schema: [docs/graph.md](docs/graph.md), or run `stakk docs graph`.

### `stakk docs [topic]`

Print the documentation bundled into the binary.
Because it ships inside the binary, it always describes the version you are actually running.
Run `stakk docs` with no arguments for the list of topics.
Each topic is one of the Markdown files in [docs/](docs/), which is also where to read them on GitHub.

At a terminal the prose is re-flowed to your terminal width.
Redirected, the source is emitted verbatim —
so `stakk docs agents >> AGENTS.md` writes exactly the Markdown in `docs/agents.md`,
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

Submission runs as analyze → plan → execute, with every repository and GitHub mutation confined to the last phase.
That is why `--dry-run` can stop after the plan and be guaranteed to have written nothing.

## Stability

stakk follows semantic versioning.
Stable surface: subcommands, their aliases and their flags, `STAKK_` environment variables,
config keys and their defaults, the two `schema_version`-ed JSON documents
(`stakk graph` and `--bookmark-command`'s stdin), diagnostic codes, and exit codes.
Free to change in any release: rendered `--help` and `stakk docs` text, error and progress *wording*,
the `pretty` output of `stakk graph`, the order of `stacks[]` in its JSON, and the TUI layout and keybindings.
Raising the minimum supported jj version is not a breaking change.

The contract itself — every entry, with the reasoning: [docs/stability.md](docs/stability.md),
or run `stakk docs stability`.

## License

MIT OR Apache-2.0
