# Configuration

stakk loads settings from TOML config files, environment variables, and CLI flags.

The full precedence order, highest to lowest:

1. **CLI flags** — `--remote`, `--pr-mode`, etc.
2. **Environment variables** — `STAKK_REMOTE`, `STAKK_PR_MODE`, etc.
3. **Repository config** — `stakk.toml`, found by walking up from the current directory
4. **User config** — `~/.config/stakk/config.toml` (Linux),
   `~/Library/Application Support/stakk/config.toml` (macOS),
   `%APPDATA%\stakk\config\config.toml` (Windows)
5. **Built-in defaults**

## Config files

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

## Config file format

All fields are optional.
Absent fields fall back to the next level in the precedence chain.
Unknown fields cause a parse error, so typos are caught early.

```toml
# stakk.toml — example with all available fields

# Git remote to push to (default: "origin")
remote = "origin"

# Extra host to treat as GitHub, for GitHub Enterprise Server
# (default: none — only github.com is accepted)
github_host = "github.example.com"

# PR creation mode: "regular" or "draft" (default: "regular")
pr_mode = "draft"

# Path to a custom minijinja template for stack comments
# (default: none — the built-in template is used)
template_path = "/path/to/my-template.md.jinja"

# Where to place stack info: "comment", "body", "none", or "ignore"
# (default: "comment")
# "none" writes no stack info and removes existing stack comments/body
# fences on the next submit (e.g. if you rely on GitHub's native
# stacked PRs). "ignore" writes no stack info and leaves existing
# artifacts untouched.
stack_placement = "body"

# Prefix for auto-generated bookmark names (default: none)
auto_prefix = "gb-"

# Revset for discovering bookmarks
# (default: "mine() ~ trunk() ~ immutable()")
# Note: the default excludes bookmarks on immutable commits. Stale
# untracked remote bookmarks can pin commits immutable — clean them up
# with `jj bookmark forget --include-remotes 'glob:<pattern>'`, or drop
# the `~ immutable()` term for one run to include them.
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

## The `inherit` field

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

## Examples

**User config** — personal defaults across all repos:

```toml
# ~/.config/stakk/config.toml
pr_mode = "draft"
stack_placement = "body"
```

**Repo config** — override the remote for this repo, inherit everything else:

```toml
# stakk.toml (in repo root)
remote = "upstream"
```

With both files above, `stakk submit` uses `remote = "upstream"` from the repo config and `pr_mode = "draft"`,
`stack_placement = "body"` from the user config.
Passing `--pr-mode regular` on the command line overrides all of them.

**Team-enforced config** — no user config inheritance:

```toml
# stakk.toml (in repo root)
inherit = false
remote = "origin"
pr_mode = "regular"
stack_placement = "comment"
```

## GitHub Enterprise Server

github.com is always accepted.
To work against a GitHub Enterprise Server host, name that host — stakk then accepts remotes on it,
and talks to its REST API at `https://<host>/api/v3`.

The host is resolved highest to lowest:

1. `--github-host <host>` — a global flag, so it works either side of the subcommand,
   e.g. `stakk show --github-host <host>`
2. `STAKK_GITHUB_HOST`
3. `github_host` in `stakk.toml`
4. `GH_HOST` — the GitHub CLI's own setting, so an existing `gh` setup needs no stakk configuration

Any other host is rejected, so an unrelated forge is never mistaken for GitHub.

The token is resolved for the host the remote actually points at,
by asking `gh auth token --hostname <host>` first and reading the environment only if `gh` is unavailable
or has no token for that host:

| Host | Environment variables |
|------|-----------------------|
| `github.com` | `GITHUB_TOKEN`, `GH_TOKEN` |
| anything else | `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` |

So a github.com token is never sent to an Enterprise host, or the other way around.

```sh
export GH_HOST=github.example.com
gh auth login --hostname github.example.com
gh auth status --hostname github.example.com
```

`gh auth status --hostname <host>` reports the token stakk will get, since stakk asks `gh` first,
so it is the quickest way to confirm the setup.
A `stakk submit --dry-run` *with selection flags* — for example `stakk submit --dry-run --keep <bookmark>` —
exercises the whole remote → token → API chain read-only; without them it stops at the selection TUI,
before the plan phase that queries the forge.
Both are covered by `stakk docs auth`.

Note that the API base is always `https`, even for an `http://` remote:
an Enterprise Server reachable only over plain HTTP is not supported.

## Environment variables

| Variable | Description |
|----------|-------------|
| `STAKK_CONFIG` | Path to config file, overrides automatic discovery (overridden by `--config`) |
| `STAKK_REMOTE` | Default git remote to push to (overridden by `--remote`) |
| `STAKK_GITHUB_HOST` | Extra host to treat as GitHub, for GitHub Enterprise Server (overridden by `--github-host`) |
| `STAKK_PR_MODE` | PR creation mode: `regular` or `draft` (overridden by `--pr-mode`) |
| `STAKK_TEMPLATE_PATH` | Path to a custom minijinja template for stack comments (overridden by `--template-path`) |
| `STAKK_STACK_PLACEMENT` | Where to place the stack info: `comment` (default), `body`, `none`, or `ignore` (overridden by `--stack-placement`) |
| `STAKK_AUTO_PREFIX` | Prefix for auto-generated bookmark names (overridden by `--auto-prefix`) |
| `STAKK_SYNC_PR_CONTENT` | Sync PR title/body from commits: `none` (default), `title`, `body`, or `all` (overridden by `--sync-pr-content`) |
| `STAKK_TRAILERS` | Whether to keep or strip git commit trailers in PR bodies: `keep` (default) or `strip` (overridden by `--trailers`) |
| `STAKK_BOOKMARK_COMMAND` | Shell command for generating custom bookmark names (overridden by `--bookmark-command`) |
| `STAKK_BOOKMARKS_REVSET` | Revset for discovering bookmarks (overridden by `--bookmarks-revset`) |
| `STAKK_HEADS_REVSET` | Revset for discovering unbookmarked heads (overridden by `--heads-revset`) |
| `GH_HOST` | The GitHub CLI's host setting; used as the `github_host` fallback |
| `GITHUB_TOKEN` | GitHub personal access token for github.com (see `stakk docs auth`) |
| `GH_TOKEN` | Alternative to `GITHUB_TOKEN` |
| `GH_ENTERPRISE_TOKEN` | Access token for a GitHub Enterprise Server host |
| `GITHUB_ENTERPRISE_TOKEN` | Alternative to `GH_ENTERPRISE_TOKEN` |

`--dry-run` and the selection flags (`--keep`, `--new`, `--new-auto`, `--new-command`) deliberately have no environment
variables or config keys: they are per-invocation decisions, and a persisted default would be surprising.
