<!--- stakk-docs
summary: Config files, precedence, and environment variables.
--->

# Configuration

Precedence, highest first:

1. **CLI flags** — `--remote`, `--pr-mode`, …
2. **Environment variables** — `STAKK_REMOTE`, `STAKK_PR_MODE`, …
3. **Repository config** — `stakk.toml`, found by walking up from the current directory to the jj workspace root
   (the directory containing `.jj/`).
   `--config <path>` or `STAKK_CONFIG` replaces it, which is also how to share one file across repos.
4. **User config** — `~/.config/stakk/config.toml` (Linux),
   `~/Library/Application Support/stakk/config.toml` (macOS),
   `%APPDATA%\stakk\config\config.toml` (Windows)
5. **Built-in defaults**

Repo and user config are merged field by field, the repo winning, unless the repo config sets `inherit = false`,
which drops the user config entirely.
All fields are optional; unknown fields are a parse error.

## Repo config runs with your privileges

A `stakk.toml` is usually committed, so cloning a repository hands you its configuration with no trust step.
Two keys act:

- **`bookmark_command`** runs through `sh -c` (`cmd /C` on Windows) during selection —
  before any GitHub call, under `--dry-run`, and with no valid token.
- **`template_path`** reads any path, inside the repo or not, and renders it into a pull request.

The rest only set preferences: closed value sets, revsets passed to `jj` as arguments, a remote that has to exist.
Before running stakk in a repository you have not read, check those two keys.
`--config <path>` swaps in a file you chose.
Note that `inherit = false` also stops your user config from overriding the repo's values;
only a flag or an environment variable can.

## All fields

```toml
# stakk.toml — every field, with its default

# Git remote to push to (default: "origin")
remote = "origin"

# Which forge the remote is on: "github" or "forgejo" (default: unset —
# the remote's host decides, see "Which forge" below). Set it to skip
# that lookup.
forge = "forgejo"

# Hosts and their forges, for GitHub Enterprise Server and self-hosted
# Forgejo (default: none; github.com and codeberg.org are built in).
# Keep the port of an http(s) remote, so "localhost:3000" works.
hosts = { "ghe.example.com" = "github", "git.example.com" = "forgejo" }

# "regular" (default) or "draft"
pr_mode = "draft"

# Custom minijinja template for stack comments (default: built-in).
# Reads any path — see the trust note above.
template_path = "/path/to/my-template.md.jinja"

# Where the stack overview lives: "comment", "body", "none", "ignore",
# "auto-comment" (default) or "auto-body". "none" writes nothing and
# removes existing stack comments and body fences; "ignore" writes
# nothing and touches nothing. The auto modes behave like "none" on
# runs where a native stack is in effect and like "comment"/"body"
# otherwise, so the default equals "comment" while native_stacks is
# "ignore". Details: stakk docs template
stack_placement = "body"

# Register stacks with GitHub's native stacked pull requests (public
# preview): "on" (fail where unavailable), "auto" (register where
# available, skip silently where not), "none" (register nothing and
# dissolve standing server-side stacks) or "ignore" (default; never
# touch the stack API). The default is "ignore" while the feature is a
# public preview and may become "auto" in a minor release once it
# leaves it — the one default exempt from the stability contract.
# A single-PR submission is not a stack and skips the registration;
# "none" retires its stacks anyway.
native_stacks = "auto"

# Prefix for auto-generated bookmark names (default: none)
auto_prefix = "gb-"

# Revset for discovering bookmarks
# (default: "mine() ~ trunk() ~ immutable()")
# Stale untracked remote bookmarks can pin commits immutable; clean
# them up with `jj bookmark forget --include-remotes 'glob:<pattern>'`
# or drop the `~ immutable()` term for one run.
bookmarks_revset = "mine() ~ trunk() ~ immutable()"

# Revset for discovering unbookmarked heads
# (default: "heads((mine() ~ empty() ~ immutable()) & trunk()..)")
heads_revset = "heads((mine() ~ empty() ~ immutable()) & trunk()..)"

# Update existing PRs from commits: "none" (default), "title", "body"
# or "all"
sync_pr_content = "all"

# Commit trailers in PR bodies: "keep" (default) or "strip"
trailers = "strip"

# Shell command for custom bookmark names — see the trust note above
bookmark_command = "my-bookmark-namer"

# Merge with the user config (default: true). Only meaningful in a
# repo config; false makes the repo file standalone, for team-enforced
# settings.
inherit = true
```

## Which forge

`stakk submit` decides the forge from the remote's host, in this order:

1. `--forge github|forgejo` (`STAKK_FORGE`, `forge` in `stakk.toml`) — explicit, and wins over everything below.
2. The built-in hosts: github.com is GitHub and codeberg.org is Forgejo.
   bitbucket.org and gitlab.com are recognised too, and refused by name — stakk does not submit to them.
3. The host table: `--host HOST=FORGE`
   (repeatable; `STAKK_HOSTS` takes a comma-separated list),
   then `hosts` in `stakk.toml`, then the GitHub CLI's `GH_HOST` as an implicit GitHub entry.
   A later layer wins for the same host, and within `--host` the last entry does.
   Hosts match case-insensitively and include the port of an `http(s)` remote.
4. A probe, for a host none of the above names — by `stakk submit` only, never by `stakk graph`.
   Three unauthenticated `GET`s go out at once, no redirects followed, five seconds at most:
   `https://<host>/api/v3/meta` is GitHub when the response carries an `X-GitHub-Request-Id` header,
   whatever its status; `<scheme>://<host>/api/v1/version` is Forgejo
   (or Gitea)
   on a `200` JSON body with a `version` string, or on the `403` a sign-in-only instance answers every API call with;
   `<scheme>://<host>/api/v4/version` is GitLab when the response carries `X-Gitlab-Meta`, and is refused by name.
   One supported answer decides, and stakk prints the `hosts` entry that skips the probe next time.
   No answer, or two, is an error — `stakk::detect::unknown_forge`, `stakk::detect::ambiguous_forge` —
   that lists what each URL returned; a host behind a login page or a proxy usually needs naming.

The remote URL decides the host and a rule only says what that host runs,
so a repository with both a github.com and an Enterprise remote works, each against its own host.
GitHub is reached at `https://<host>/api/v3` — always `https`, even for an `http://` remote —
and Forgejo at `<scheme>://<host>/api/v1` with the remote URL's own scheme and port,
so a plain-`http` instance on a port works.
Tokens are resolved per host and never sent across hosts: `stakk docs auth`.

`stakk graph` is offline and never decides a forge: it reports each remote's host and leaves it at that.

## Environment variables

| Variable | Description |
|----------|-------------|
| `STAKK_CONFIG` | Path to config file, replaces automatic discovery (overridden by `--config`) |
| `STAKK_REMOTE` | Git remote to push to (overridden by `--remote`) |
| `STAKK_FORGE` | `github` or `forgejo`; unset means the remote's host decides (overridden by `--forge`) |
| `STAKK_HOSTS` | Comma-separated `HOST=FORGE` list, e.g. `ghe.example.com=github,git.example.com=forgejo` (overridden by `--host`) |
| `STAKK_PR_MODE` | `regular` or `draft` (overridden by `--pr-mode`) |
| `STAKK_TEMPLATE_PATH` | Custom minijinja template for stack comments (overridden by `--template-path`) |
| `STAKK_STACK_PLACEMENT` | `comment`, `body`, `none`, `ignore`, `auto-comment` (default) or `auto-body` (overridden by `--stack-placement`) |
| `STAKK_NATIVE_STACKS` | `on`, `auto`, `none` or `ignore` (default) (overridden by `--native-stacks`) |
| `STAKK_AUTO_PREFIX` | Prefix for auto-generated bookmark names (overridden by `--auto-prefix`) |
| `STAKK_SYNC_PR_CONTENT` | `none` (default), `title`, `body` or `all` (overridden by `--sync-pr-content`) |
| `STAKK_TRAILERS` | `keep` (default) or `strip` (overridden by `--trailers`) |
| `STAKK_BOOKMARK_COMMAND` | Shell command for custom bookmark names (overridden by `--bookmark-command`) |
| `STAKK_BOOKMARKS_REVSET` | Revset for discovering bookmarks (overridden by `--bookmarks-revset`) |
| `STAKK_HEADS_REVSET` | Revset for discovering unbookmarked heads (overridden by `--heads-revset`) |
| `GH_HOST` | The GitHub CLI's host setting; an implicit `<GH_HOST>=github` entry below every other one |
| `GH_TOKEN`, `GITHUB_TOKEN` | Token for github.com, in that order (see `stakk docs auth`) |
| `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` | Token for any other GitHub host, in that order |
| `FORGEJO_TOKEN` | Token for any Forgejo host |

`STAKK_GITHUB_HOST` is retired in favour of `STAKK_HOSTS`; `stakk submit` warns while it is set.

`--dry-run` and the selection flags (`--keep`, `--new`, `--new-auto`, `--new-command`) have no environment variables
or config keys on purpose: they are per-invocation decisions.
