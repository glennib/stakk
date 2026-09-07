<!--- stakk-docs
summary: GitHub authentication: tokens, hosts, and troubleshooting.
--->

# Authentication

stakk resolves a GitHub API token per host on every run; there is no stakk login and nothing stored.
If the GitHub CLI is authenticated for the host your remote points at, stakk is too.

Pushing is separate: `jj git push` uses your normal git credentials.
The token covers only the API calls that create, update and comment on pull requests.

## Which host

The remote URL decides the host.
`github.com` is always accepted; any other host must be named first, via `--github-host`, `STAKK_GITHUB_HOST`,
`github_host` in `stakk.toml`, or the GitHub CLI's `GH_HOST` (precedence: `stakk docs config`).
The setting only allows a host and never overrides the URL,
so a repo with both a github.com remote and an Enterprise remote works, each against its own host.

## How the token is resolved

1. `gh auth token --hostname <host>` — `gh` answers with the environment token for that host if one is set,
   otherwise with its stored credential.
2. If `gh` is not installed or has no token for the host, stakk reads the host's variables itself:

| Host | Variables, in precedence order |
|------|--------------------------------|
| `github.com` | `GH_TOKEN`, `GITHUB_TOKEN` |
| anything else | `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` |

The order is the one `gh help environment` documents, so both paths agree on which variable wins.

- An exported token overrides `gh`'s stored credential immediately; no need to log out.
- Empty variables are skipped, and a missing or logged-out `gh` is not an error.
- Nothing is validated at resolution time: an expired token fails at the first API call.
- `gh` treats subdomains of `ghe.com` as github.com, stakk treats them as Enterprise.
  Set `GH_ENTERPRISE_TOKEN` there.

A classic personal access token needs the `repo` scope;
a fine-grained token needs write access to pull requests on the repositories you submit from.

## Checking the setup

`gh auth status --hostname <host>` names the token stakk will get.

`stakk submit --dry-run --keep <bookmark>` exercises the chain read-only:
a missing token fails before the TUI would open, a rejected one fails in the plan phase when the API is first called.
Without selection flags the command stops at the TUI (or `stakk::not_interactive` without a terminal).
`stakk graph` is offline and checks nothing.

## GitHub Enterprise Server

```sh
export GH_HOST=github.example.com
gh auth login --hostname github.example.com
gh auth status --hostname github.example.com
```

stakk then accepts remotes on that host and uses `https://<host>/api/v3`.
The API base is always `https`, even for an `http://` remote; plain-HTTP servers are not supported.

## When it fails

- **`stakk::auth::no_token`** — no token for the host.
  The usual cause is a token set for the *other* kind of host.
  `gh auth login --hostname <host>`, or set that host's variables.
- **`stakk::auth::gh_cli_error`** — `gh` was found but could not be started; repair the installation.
  A missing `gh` is fine, a broken one is not.
- **401 or 403 from the API** — a token was resolved and GitHub rejected it: expired, revoked, or missing the scope.
  `gh auth status --hostname <host>` names its source.
