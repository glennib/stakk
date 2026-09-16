<!--- stakk-docs
summary: Forge authentication: tokens, hosts, and troubleshooting.
--->

# Authentication

stakk resolves a forge API token per host on every run; there is no stakk login and nothing stored.
On GitHub, if the GitHub CLI is authenticated for the host your remote points at, stakk is too.
Forgejo has its own section below.

Pushing is separate: `jj git push` uses your normal git credentials.
The token covers only the API calls that create, update and comment on pull requests.

## Which host

The remote URL decides the host, and the host decides the forge: `github.com` is GitHub, `codeberg.org` is Forgejo,
and any other host is looked up in the host table — `--host HOST=FORGE`, `STAKK_HOSTS`, `hosts` in `stakk.toml`,
or the GitHub CLI's `GH_HOST` as a GitHub entry (order and details: `stakk docs config`).
Any other host is probed once, unauthenticated, and recognised by how its API answers
(`stakk docs config`); `--forge` overrides all of that for one run.
A rule only says what a host runs and never changes the URL,
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

stakk then takes that host as GitHub — `GH_HOST` doubles as a host-table entry,
the same as `--host github.example.com=github` — and uses `https://<host>/api/v3`.
The API base is always `https`, even for an `http://` remote; plain-HTTP servers are not supported.

## Forgejo

```sh
export FORGEJO_TOKEN=...
stakk submit --dry-run --keep <bookmark>
```

The token is read from `FORGEJO_TOKEN` and nothing else: `gh` is not consulted,
and none of the GitHub variables are read, so a GitHub token is never sent to a Forgejo host.
There is no per-host split; with two Forgejo instances, set the variable per shell.

Mint the token under **Settings → Applications** on the instance, with the scopes `read:repository`,
`write:repository` and `write:issue`
(pull requests are issues in Forgejo's data model, and stack comments are issue comments).

`codeberg.org` needs no setting.
A self-hosted instance is named once with `--host <host[:port]>=forgejo`, `STAKK_HOSTS` or `hosts` in `stakk.toml`;
the API is reached at `<scheme>://<host>/api/v1` with the remote URL's own scheme and port,
so a plain-`http` instance on a port works.
An SSH remote is taken to mean `https`.

## When it fails

- **`stakk::auth::no_token`** — no GitHub token for the host.
  The usual cause is a token set for the *other* kind of host.
  `gh auth login --hostname <host>`, or set that host's variables.
- **`stakk::auth::no_forgejo_token`** — `FORGEJO_TOKEN` is unset or empty for a Forgejo host.
- **`stakk::detect::unknown_forge`** — the remote's host is neither built in nor in the host table,
  and the probe got no answer that looks like GitHub or Forgejo; the message lists what each URL returned.
  Name the host the way the help says, and it stays known.
- **`stakk::detect::ambiguous_forge`** — the probe got answers that look like both, which a proxy answering
  every path can cause; name the host the same way.
- **`stakk::detect::unsupported_forge`** — the host runs GitLab or Bitbucket Cloud, which stakk does not submit to.
- **`stakk::auth::gh_cli_error`** — `gh` was found but could not be started; repair the installation.
  A missing `gh` is fine, a broken one is not.
- **401 or 403 from the API** — a token was resolved and the forge rejected it: expired, revoked, or missing a scope.
  On GitHub, `gh auth status --hostname <host>` names its source.
