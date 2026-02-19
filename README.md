# stakk

**stakk** bridges [Jujutsu](https://github.com/jj-vcs/jj) bookmarks to GitHub
stacked pull requests.

It is not a jj wrapper. It complements jj by reading your local bookmark state
and turning it into a coherent set of GitHub PRs that merge into each other in
the correct order — with stack-awareness comments, correct base branches, and
idempotent updates.

## Origins

stakk is inspired by [jj-stack](https://github.com/keanemind/jj-stack), a
TypeScript/ReScript CLI that does the same job. jj-stack's core algorithms —
change graph construction, segment grouping, topological ordering — directly
informed stakk's design.

stakk reimplements these ideas in Rust to address architectural limitations
that made certain improvements difficult in jj-stack: workspace and
non-colocated repo support, concurrent API calls, draft PRs, PR bodies from
commit descriptions, and structured error diagnostics.

## Installation

> **TODO**: Distribution is not yet set up. For now, clone the repository and
> build from source with `cargo build --release --bin stakk`.

## Quick start

```
# See your bookmark stacks
stakk

# Submit a bookmark (and its ancestors) as stacked PRs
stakk submit my-feature

# Preview what would happen without doing anything
stakk submit my-feature --dry-run

# Create PRs as drafts
stakk submit my-feature --draft
```

## How stacking works

In jj, you create bookmarks that point at changes. When bookmarks form a
linear chain — each building on the previous — they represent a stack:

```
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

Each PR shows only its own diff, and a stack comment links all related PRs
together:

```
### Stack (3 bookmarks, base: main)

1. https://github.com/you/repo/pull/1
2. https://github.com/you/repo/pull/2
3. **https://github.com/you/repo/pull/3 ← this PR**
```

Re-running `stakk submit` is always safe — it updates existing PRs rather
than creating duplicates.

## Usage

### `stakk` (no arguments)

Shows repository status: default branch, remotes, and all bookmark stacks
with their commit summaries.

```
Default branch: main
Remote: origin git@github.com:you/repo.git (you/repo)

Stacks (2 found):
  Stack 1:
    feat-auth (1 commit(s)): add authentication layer
    feat-api (2 commit(s)): implement API endpoints
  Stack 2:
    fix-typo (1 commit(s)): fix typo in README
```

### `stakk submit <bookmark>`

Submit a bookmark and all its ancestors as stacked PRs.

| Flag | Description |
|------|-------------|
| `--dry-run` | Show the submission plan without executing |
| `--draft` | Create new PRs as drafts |
| `--remote <name>` | Push to a specific remote (default: `origin`) |

PR titles come from the first line of the jj change description. PR bodies
are populated from the full description (everything after the title line).
For segments with multiple commits, descriptions are joined with `---`
separators. Bodies are only set on PR creation — manually edited PR bodies
are never overwritten.

### `stakk auth test`

Validate that GitHub authentication is working and print the authenticated
username.

### `stakk auth setup`

Print instructions for setting up authentication. stakk resolves a GitHub
token in this order:

1. **GitHub CLI** (`gh auth token`) — recommended
2. **`GITHUB_TOKEN`** environment variable
3. **`GH_TOKEN`** environment variable

## Design

stakk never calls `git` directly. All git operations go through `jj`
subcommands (`jj git push`, `jj git remote list`, etc.). This means stakk
works automatically in jj workspaces and non-colocated repositories — two
cases where calling `git` directly fails.

All forge interaction goes through a `Forge` trait. GitHub is the first (and
currently only) implementation, but the core submission logic is
forge-agnostic. This opens the door to Forgejo, GitLab, or other platforms
in the future.

The submission pipeline is split into three phases:

- **Analyze** — pure function, no I/O, fully testable with mock data
- **Plan** — queries the forge for existing PRs, determines actions
- **Execute** — pushes bookmarks, creates/updates PRs, manages comments

This separation makes the business logic testable without hitting real APIs,
and `--dry-run` falls out naturally (run phases 1 and 2, skip 3).

## License

MIT
