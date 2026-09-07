# stakk

**stakk** turns [Jujutsu](https://github.com/jj-vcs/jj) bookmarks into GitHub stacked pull requests.
Pick a stack, name the bookmarks that still need one, and stakk pushes and maintains one PR per bookmark:
correct base branches, a stack overview on every PR, no duplicates on re-runs.
It works with
[GitHub's native stacked pull requests](https://docs.github.com/en/pull-requests/how-tos/stacked-pull-requests): opt in
with `--native-stacks auto` and GitHub renders the stack itself and retargets PRs as the stack merges.

jj stays in charge of your commits and bookmarks; stakk acts only where that state has to exist on GitHub.
It never calls `git` — everything goes through `jj`, so workspaces and non-colocated repos just work.

![Interactive stakk submission flow](media/stakk.gif)

## Installation

Requires the [`jj`](https://github.com/jj-vcs/jj) CLI on your `PATH`, version 0.39.0 or newer.

```shell
mise use -g 'github:glennib/stakk'   # recommended
mise use -g 'cargo:stakk'            # from crates.io
cargo binstall stakk
cargo install stakk
```

Or download a binary from the [latest release](https://github.com/glennib/stakk/releases/latest).

## Quick start

```shell
stakk          # pick a stack and assign bookmarks in the TUI
stakk graph    # show your stacks and their change ids (offline)

# Without the TUI: one mark per PR boundary
stakk submit --keep feat-auth --new qzvs=feat-api --new-auto wmtk --dry-run
```

`--dry-run` prints the plan and stops: no bookmark is created, nothing is pushed, nothing is written to GitHub.
`stakk --help` and `stakk <subcommand> --help` are the flag reference;
`stakk docs` prints the same documentation as [docs/](docs/), bundled into the binary.

## How stacking works

Bookmarks that form a linear chain are a stack:

```text
 ○  feat-ui    ← leaf
 ○  feat-api
 ○  feat-auth
 ◆  main       ← trunk
```

Submitting all three — in the TUI, or as `stakk submit --keep feat-auth --keep feat-api --keep feat-ui` —
pushes each bookmark and creates or updates one PR per bookmark, based on the one below it: `feat-auth` → `main`,
`feat-api` → `feat-auth`, `feat-ui` → `feat-api`.
Each PR shows only its own diff.

Unbookmarked commits can become PRs too.
In the TUI each commit cycles through `[x]` existing bookmark, `[~]` name derived from the commit (TF-IDF), `[>]` typed,
`[+]` generated `stakk-<change_id>`, `[*]` from `--bookmark-command`, and `[ ]` skip.
`--auto-prefix` brands the derived names (`gb-caching-database`).

## GitHub native stacks

`--native-stacks` registers the submitted stack with GitHub's native stacked pull requests
(public preview), so GitHub draws the stack on every PR and retargets the remaining PRs as the stack merges bottom-up.
Modes: `on` (fail where the feature is unavailable), `auto`
(register where available, skip where not — GitHub Enterprise Server, for example),
`none` (register nothing and dissolve the server-side stacks stakk's PRs are in), and `ignore`
(never touch the stack API).
The default is `ignore` until the feature reaches general availability, then `auto`.

Native stacks layer on top of the base-branch chain stakk maintains anyway; nothing else about a submission changes.

## Stack info placement

Every PR in a stack also gets a stack overview from stakk, leaf at the top like `stakk graph` and the TUI.
Each row is a bare PR link, which GitHub renders with the PR's live title and merge state:

```text
Stack of 3 PRs merging into main

• Add the search UI #14 (top of stack)
• Add the search API #13 👈 this PR
• Add user authentication #12
• main
```

`--stack-placement` decides where it lives: a PR `comment`, a fenced section in the PR `body`, `none`
(write nothing and remove what is there),
`ignore` (write nothing, touch nothing), or `auto-comment` (default) / `auto-body`,
which write like `comment`/`body` except on runs where a native stack is in effect,
where GitHub's rendering replaces it.
So `--native-stacks auto` gives native rendering where available and stack comments everywhere else, never both.
Switching between `comment` and `body` migrates automatically.

Mode table and [minijinja](https://github.com/mitsuhiko/minijinja) templating:
[docs/template.md](docs/template.md) or `stakk docs template`.

## Configuration

CLI flags override `STAKK_*` environment variables, which override a repository `stakk.toml`,
which overrides the user config (`~/.config/stakk/config.toml` on Linux).

```toml
# stakk.toml
remote = "origin"
pr_mode = "draft"
stack_placement = "body"
auto_prefix = "gb-"
```

Every key and variable: [docs/config.md](docs/config.md) or `stakk docs config`.

### GitHub Enterprise Server

Name the host with `--github-host`, `STAKK_GITHUB_HOST`, `github_host` in `stakk.toml`, or `GH_HOST`;
stakk then accepts remotes on it and uses `https://<host>/api/v3`.
Tokens are resolved per host like the GitHub CLI does it, so an Enterprise token is never sent to github.com.
Setup and troubleshooting: [docs/auth.md](docs/auth.md) or `stakk docs auth`.

## Non-interactive selection

`--keep`, `--new`, `--new-auto` and `--new-command` replace the TUI with an explicit selection:
every PR boundary is named on the command line, all marks lie on one trunk-to-tip path,
unmarked commits fold into the PR above them, and anything above the topmost mark is not submitted.
`rev` is any jj revset naming one commit: `@-`, a bookmark, or a change id from `stakk graph --format=json`.

Rules and diagnostic codes, written for coding agents: [docs/agents.md](docs/agents.md) or `stakk docs agents`
(`stakk docs agents >> AGENTS.md` hands it to yours).
Exit codes and a worked Python example: [docs/scripting.md](docs/scripting.md) or `stakk docs scripting`.
The JSON schema: [docs/graph.md](docs/graph.md) or `stakk docs graph`.

## PR titles and bodies

The title is the first line of the change description, the body is the rest;
multi-commit segments join their descriptions with `---`.
Hard-wrapped prose is reflowed into paragraphs; Markdown structure passes through verbatim.
Both are written only on PR creation, so edits on GitHub survive — `--sync-pr-content`
(`title`, `body`, `all`) opts into updating them.
`--trailers strip` drops the trailing `Key: value` block (`Signed-off-by`, `Refs`, …).

## Custom bookmark names

`--bookmark-command` runs a program (`sh -c`, or `cmd /C` on Windows) with a JSON description of the segment on stdin
and takes its stdout as the bookmark name.
It powers the `[*]` TUI state and `--new-command`; the JSON schema is in `stakk submit --help`.

## Immutable commits

Commits jj considers immutable cannot get a new bookmark: the default bookmarks revset excludes `immutable()`,
so stakk would create a PR it could never see again.
The TUI locks such rows, `--new <rev>` fails with `stakk::selection::rev_immutable`, and `stakk graph` marks them.
Move the work onto mutable commits, or create the bookmark yourself and drop `~ immutable()` from `--bookmarks-revset`.

## Stability

stakk follows semantic versioning.
Stable: subcommands, aliases and flags, `STAKK_` environment variables, config keys and defaults,
the two `schema_version`-ed JSON documents (`stakk graph` and `--bookmark-command`'s stdin), and exit codes.
Free to change: `--help` and `stakk docs` text, error wording, the set of diagnostic codes,
`stakk graph`'s pretty output, the order of `stacks[]`, the TUI, and the minimum jj version.
The contract: [docs/stability.md](docs/stability.md) or `stakk docs stability`.

## License

MIT OR Apache-2.0
