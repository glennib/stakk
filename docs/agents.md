<!--- stakk-docs
summary: Submitting without the TUI, written for coding agents.
--->

# Driving stakk from a coding agent

`stakk graph` reports the state, `stakk submit` with selection flags acts on it.
`stakk graph` is offline — jj only, never GitHub — so run it freely.

## Reading the state

```console
stakk graph --format=json
```

Each segment lists its bookmarks with a `remote_state`:

| `remote_state` | Meaning | A submission… |
|----------------|---------|---------------|
| `unpushed` | No remote counterpart | pushes it for the first time |
| `diverged` | A tracked remote sits elsewhere | moves the remote to your commit |
| `synced` | Some remote is on this commit | pushes nothing, given one remote |

This comes from jj alone and says nothing about pull requests;
stakk learns which PRs exist in `stakk submit`'s plan phase.
`synced` means "on some remote", not "on the one you push to" — with several remotes, check `remotes[]`.

## Naming boundaries

```console
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk --dry-run
```

| Flag | Meaning |
|------|---------|
| `--keep <bookmark>` | An existing bookmark stays a PR boundary |
| `--new <rev>[=<name>]` | New bookmark at `rev`, named `name` or `stakk-<change_id>` |
| `--new-auto <rev>` | New TF-IDF-named bookmark at `rev`, honoring `--auto-prefix` |
| `--new-command <rev>` | New bookmark at `rev`, named by `--bookmark-command` |

All are repeatable and CLI-only: no environment variables, no config keys.

`rev` is a jj revset that resolves to exactly one commit, handed to `jj log -r` verbatim:
a `change_id` or `short_change_id` from `stakk graph`, `@-`, a bookmark name, any revset expression.
Use `change_id` for anything stored — a short id is unique only against the repository right now.
In `--new <rev>=<name>` the name starts at the first `=` outside parentheses and quotes,
so `remote_bookmarks(main, remote=origin)=<name>` works.

Submit flags go after the subcommand: `stakk submit --dry-run`, not `stakk --dry-run submit`.
Only `--config` and `--github-host` are accepted on either side.

## What the marks decide

- **Marks fully determine the PR set.**
  A commit is a PR boundary only if it is marked.
- **All marks lie on one trunk-to-tip path**, and the topmost mark is the tip.
  Anything else is `stakk::selection::not_colinear`.
- **Unmarked commits below the topmost mark fold into the PR above them.** Fewer marks, fewer PRs.
- **Commits above the topmost mark are not submitted.**
  A last segment with an empty `bookmarks[]` is an unbookmarked head; mark its tip with `--new`,
  `--new-auto` or `--new-command` to include it.

## Previewing

`--dry-run` prints the planned bookmark creations and PR actions, then stops.
It creates no bookmark, pushes nothing and writes nothing to GitHub, though the plan phase does read GitHub.
Not covered: a configured `--bookmark-command` still runs during selection,
and the execute-phase effects of `--stack-placement none` (removing stack artifacts) and `--native-stacks`
(creating, dissolving or retiring server-side stacks) are not previewed.

## Diagnostics

Selection failures carry machine-readable codes and leave the repository untouched.

| Code | Meaning |
|------|---------|
| `stakk::selection::rev_unresolvable` | jj rejected the revset; the message carries jj's diagnosis |
| `stakk::selection::rev_not_found` | The revset selects no commit |
| `stakk::selection::rev_not_unique` | The revset selects more than one commit |
| `stakk::selection::rev_not_on_stack` | The commit is on no submittable stack: trunk, immutable, revset-excluded, or an empty `@` (try `@-`) |
| `stakk::selection::rev_immutable` | The commit is jj-immutable — see below |
| `stakk::selection::empty_rev` | A selection flag got an empty `REV`, e.g. from a shell expansion |
| `stakk::selection::invalid_new_spec` | A `--new` value is neither `REV` nor `REV=NAME` |
| `stakk::selection::not_colinear` | The marks are not on one trunk-to-tip path; each entry in `stacks[]` is one |
| `stakk::selection::keep_not_found` | No such bookmark on the selected path |
| `stakk::selection::no_stacks` | The repository has no stacks to select from |
| `stakk::selection::duplicate_mark` | The same revision was marked twice; one boundary takes one mark, however many bookmarks it carries |
| `stakk::selection::duplicate_name` | Two marks would create the same bookmark name |
| `stakk::selection::name_exists` | The name is already a local bookmark |
| `stakk::selection::bookmark_command_not_configured` | `--new-command` without `--bookmark-command` |

With no marks at all, `stakk submit` means the TUI; without a terminal that is `stakk::not_interactive`, exit `1`.

## Immutable commits

jj-immutable commits cannot take a new bookmark: the default bookmarks revset excludes `immutable()`,
so stakk would create a PR it could never see or manage again.
`--new <rev>` there fails with `stakk::selection::rev_immutable`, and `stakk graph` reports `is_immutable`.
Either move the work onto mutable commits,
or create the bookmark by hand and drop `~ immutable()` from `--bookmarks-revset`.

## The `stakk graph` document

`--format=json` is sparse and has everything the selection flags need.
`--format=json-full` adds `commit_id`, `description`, `author` and `files[]` to every commit,
for reading commit messages or naming a bookmark from the work itself.
Sparse is a strict subset of full, so paths never change between them.

```text
schema_version, default_branch, excluded_bookmarks[], excluded_head_count
remotes[]        name, url, github ("owner/repo" | null)
stacks[]
  segments[]     bookmarks[] {name, remote_state}
    commits[]    oldest first: change_id, short_change_id, title,
                 committer_timestamp, is_immutable, local_bookmark_names[],
                 is_boundary, is_leaf
```

Three traps:

- **`stacks[]` has no stable order**, and a stack has no name.
  "The most recently modified stack" is the one with the highest `committer_timestamp` over its commits.
- **`committer_timestamp` is offset-aware** (`2026-02-19T19:47:54+01:00`), so compare instants, not strings.
- **`title` is empty** for a commit with no description — normal jj state, not an error.

Field by field: `stakk docs graph`.
A parsing example: `stakk docs scripting`.
