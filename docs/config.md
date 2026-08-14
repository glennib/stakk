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

## Repo config runs with your privileges

Discovery has no trust step, and a `stakk.toml` is often committed,
so cloning a repository can hand you its configuration.
Most keys only set a preference.
Two do more:

- **`bookmark_command`** is run through `sh -c` (`cmd /C` on Windows) while stakk works out which bookmarks to create.
  That happens in the first phase, so it runs before any GitHub call, with `--dry-run`, and with no valid token.
- **`template_path`** reads whatever path it names — not only files inside the repo — and renders the contents into a
  pull request comment or body, which is outward-facing.

The rest cannot act on their own.
Revsets reach `jj` as arguments rather than through a shell, `pr_mode`, `stack_placement`,
`sync_pr_content` and `trailers` are closed sets of values, `remote` has to name a remote that already exists,
and `github_host` does nothing without a remote on that host.

So before running stakk in a repository you have not read, check whether it has a `stakk.toml`,
and read those two keys if it does.
Note also that `inherit = false` in a repo config suppresses your user config entirely,
so a repo-supplied value cannot be overridden by yours — only by a flag or an environment variable.

`--config <path>` (or `STAKK_CONFIG`) replaces the repo-level file rather than adding to it,
which is the way to run in an untrusted repository with configuration you chose.

## Config file format

All fields are optional.
Absent fields fall back to the next level in the precedence chain.
Unknown fields cause a parse error, so typos are caught early.

```toml
# stakk.toml — example with all available fields

# Git remote to push to (default: "origin")
remote = "origin"

# Extra host to treat as GitHub, for GitHub Enterprise Server
# (default: unset — falls back to GH_HOST; github.com is always accepted)
github_host = "github.example.com"

# PR creation mode: "regular" or "draft" (default: "regular")
pr_mode = "draft"

# Path to a custom minijinja template for stack comments
# (default: none — the built-in template is used)
# Reads any path and renders it into a PR — see the trust note above.
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
# Runs via sh -c — see the trust note above.
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

Enforcing settings this way works because the repo file wins and your own is skipped —
which is also why a repo you have not read deserves a look first.
See **Repo config runs with your privileges** above.

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
   e.g. `stakk graph --github-host <host>`
2. `STAKK_GITHUB_HOST`
3. `github_host` in `stakk.toml`
4. `GH_HOST` — the GitHub CLI's own setting, so an existing `gh` setup needs no stakk configuration

Any other host is rejected, so an unrelated forge is never mistaken for GitHub.

The token is resolved for the host the remote actually points at,
so a github.com token is never sent to an Enterprise host, or the other way around.
The per-host variables, the `gh auth login` commands that set the host up, and how to confirm the setup afterwards:
`stakk docs auth`.

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
| `GH_TOKEN` | GitHub personal access token for github.com (see `stakk docs auth`) |
| `GITHUB_TOKEN` | Alternative to `GH_TOKEN` |
| `GH_ENTERPRISE_TOKEN` | Access token for a GitHub Enterprise Server host |
| `GITHUB_ENTERPRISE_TOKEN` | Alternative to `GH_ENTERPRISE_TOKEN` |

`STAKK_DRAFT` and `STAKK_TEMPLATE` are gone: `STAKK_PR_MODE=draft` replaces the first, `STAKK_TEMPLATE_PATH` the second.
A stale one is not an error — stakk cannot know the variable is still meant for it —
but `stakk submit` warns on stderr while either is set.
Unset it to fix the setting, or set it empty to silence the warning.

`--dry-run` and the selection flags (`--keep`, `--new`, `--new-auto`, `--new-command`) deliberately have no environment
variables or config keys: they are per-invocation decisions, and a persisted default would be surprising.
