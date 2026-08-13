# Authentication

stakk talks to the GitHub REST API with a token it resolves itself, per host, every time it runs.
There is nothing to store and no stakk-specific login:
if the GitHub CLI is already authenticated for the host your remote points at, stakk is authenticated too.

Pushing is not part of this.
Branches are pushed by `jj git push`, which uses your normal git credentials (SSH key or credential helper).
The token below is used only for the API calls that create, update and comment on pull requests.

## Which host

The **remote URL decides the host**, and the token is resolved for that host.
`github.com` is always accepted; any other host has to be named first, through `--github-host`, `STAKK_GITHUB_HOST`,
`github_host` in `stakk.toml`, or the GitHub CLI's own `GH_HOST`.
That setting only says which hosts are *allowed* — it never overrides the URL,
so a repo with both a github.com remote and an Enterprise remote works, each authenticated against its own host.

The full precedence order for those settings: `stakk docs config`.

## How the token is resolved

stakk does not read your `gh` config or keyring itself.
It **delegates to the GitHub CLI first** and only falls back to reading the environment:

1. `gh auth token --hostname <host>`.
   `gh` answers with a **token from the environment** when one is set for that host,
   and with its **stored credential** otherwise.
2. If `gh` is not installed, or has no token for the host, stakk reads the host's environment variables itself.

Either way, the variables that apply depend on the host — a github.com token is never sent to an Enterprise host,
or the other way around:

| Host | Environment variables |
|------|-----------------------|
| `github.com` | `GH_TOKEN`, `GITHUB_TOKEN` |
| anything else | `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` |

Both stakk and `gh` read them in the order listed — it is the precedence `gh help environment` documents.
So when several are set, the winner is the same whether the token came from `gh` or from stakk's own fallback.

One exception: `gh` treats a subdomain of `ghe.com` as github.com and reads `GH_TOKEN`/`GITHUB_TOKEN` there,
while stakk treats every host other than github.com as Enterprise.
On such a host the two disagree about which *variables* apply, not merely their order,
so set `GH_ENTERPRISE_TOKEN` for stakk.

Four details worth knowing:

- **An exported token takes effect immediately.**
  Because `gh auth token` returns the environment token when there is one,
  setting `GH_TOKEN` overrides the credential `gh` has stored.
  There is no need to log out of `gh` to make it apply.
- **`--hostname` is always passed to `gh`.**
  Without it `gh` would answer for whatever `GH_HOST` names, which need not be the host this repo's remote points at.
- **Empty variables are skipped**, and a `gh` that is absent or not logged in for the host is not an error —
  resolution simply falls through to stakk's own environment read.
- **Nothing is validated at resolution time.**
  An expired or revoked token resolves perfectly well and then fails at the first API call.

A classic personal access token needs the `repo` scope.
A fine-grained token needs write access to pull requests on the repositories you submit from.

## Checking the setup

`stakk` has no connectivity probe of its own; two commands cover it.

**`gh auth status --hostname <host>`** reports the token stakk will actually get: stakk asks `gh` for that host first,
so whatever `gh` names here — an environment variable or its stored credential — is what stakk uses.
It is the quickest way to confirm the setup.

```sh
gh auth status --hostname github.com
```

**`stakk submit --dry-run`** exercises the chain read-only.
The remote and the token are resolved before the selection TUI opens, so a *missing* token fails immediately,
with the diagnostic that names the problem.
Nothing is validated at that point, though — a resolved-but-rejected token surfaces only once the API is called,
and the API is not called until the plan phase, after the selection.
Bare `stakk submit --dry-run` therefore stops at the TUI
(or at `stakk::not_interactive` on a non-terminal stdin); add selection flags to reach the API:

```sh
stakk submit --dry-run --keep my-bookmark
```

`--dry-run` returns after printing the plan, so nothing is pushed, created or commented on.

`stakk show` is *not* an authentication check: it is offline and reads only jj, never the token.

## GitHub Enterprise Server

Name the host, log in to it, and confirm:

```sh
export GH_HOST=github.example.com
gh auth login --hostname github.example.com
gh auth status --hostname github.example.com
```

stakk then accepts remotes on that host and talks to its REST API at `https://<host>/api/v3`.
The API base is always `https`, even for an `http://` remote:
an Enterprise Server reachable only over plain HTTP is not supported.

## When it fails

- **`stakk::auth::no_token`** — no token was found for the host.
  Run `gh auth login --hostname <host>`, or set the variables listed above *for that host*.
  The usual cause is a token set for the wrong one — `GITHUB_TOKEN` exported while the remote is an Enterprise host,
  or vice versa.
- **`stakk::auth::gh_cli_error`** — `gh` was found but could not be started.
  A *missing* `gh` is not an error and falls through to the token variables,
  but a `gh` that fails to launch aborts resolution before they are read — repair the installation.
- **A 401 or 403 from the API** means a token *was* resolved and GitHub rejected it:
  expired, revoked, or missing the scope above.
  `gh auth status --hostname <host>` names the source, which tells you which token to replace.
