<!--- stakk-docs
summary: Submitting without the TUI, written for coding agents.
--->

# Driving stakk from a coding agent

Submitting a stack of pull requests without the TUI.

Two commands do everything: `stakk graph` reports what exists, `stakk submit` acts on it.
`stakk graph` is offline — it queries `jj` only, never GitHub — so it is cheap to run at any point.

## Reading the state

```console
stakk graph --format=json
```

Every segment reports its bookmarks with a `remote_state`,
so the document already distinguishes new work from an update:

| `remote_state` | Meaning | What a submission does |
|----------------|---------|------------------------|
| `unpushed` | No remote counterpart on any remote | Pushes it for the first time |
| `diverged` | A tracked remote sits elsewhere | Moves the remote to your commit |
| `synced` | Some remote is on the same commit | Nothing to push, given one remote |

This is derived from `jj` alone, so it describes what a *push* would do.
It says nothing about whether a pull request exists; stakk learns that during `stakk submit`'s plan phase,
which does query GitHub.

One caveat: `stakk graph` takes no `--remote`, so `synced` means "on some remote", not "on the one you push to".
In a repository with several remotes, check `remotes[]` before reading `synced` as "nothing to do" —
see `stakk docs graph`.

## Naming boundaries

```console
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk --dry-run
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk
```

The selection flags replace the TUI with a fully explicit selection.
They are deliberately CLI-only — no environment variables, no config keys —
because a persisted default would silently change what gets submitted.

| Flag | Meaning |
|------|---------|
| `--keep <bookmark>` | Keep an existing bookmark as a PR boundary (repeatable) |
| `--new <rev>[=<name>]` | New bookmark at `rev` — `stakk-<change_id>` by default, or `name` (repeatable) |
| `--new-auto <rev>` | New TF-IDF-named bookmark at `rev`, honoring `--auto-prefix` (repeatable) |
| `--new-command <rev>` | New bookmark at `rev` named by `--bookmark-command` (repeatable) |

`rev` is a jj revset that resolves to exactly one commit:
a `change_id` or `short_change_id` as printed by `stakk graph`, `@-`, a bookmark name,
or any expression `jj log -r` accepts — it is handed to jj verbatim.
A short id is only unique against the repository as it stands right now,
so one stored and used later can fail with `stakk::selection::rev_unresolvable`,
where the full `change_id` still resolves.
In `--new <rev>=<name>`, the name starts at the first `=` outside parentheses and quotes,
so a revset may carry keyword arguments (`remote_bookmarks(main, remote=origin)=<name>`).

Submit flags belong after the subcommand.
`stakk submit --dry-run` works; `stakk --dry-run submit` is rejected as an unexpected argument.
`--config` and `--github-host` are the exceptions, accepted on either side.

## What the marks decide

**The marks fully determine the PR set — nothing is implicit.**
A commit becomes a PR boundary only if it is marked.

**All marks must lie on one trunk-to-tip path**, and the topmost mark is the tip of the submission.
Marks on diverging branches are rejected with `stakk::selection::not_colinear`.

**Unmarked commits *below* the topmost mark fold into the PR above them.**
The minimal selection produces the *fewest* PRs, not the most.

**Commits *above* the topmost mark are not submitted at all.**
An unbookmarked work-in-progress head is left out silently unless it is marked.
A last segment with an empty `bookmarks` array is that head; `--new`,
`--new-auto` or `--new-command` on its tip includes it.

## Previewing

`--dry-run` prints the planned bookmark creations and PR actions, then stops.
It creates no bookmark, pushes nothing, and writes nothing to GitHub — though the plan phase does *read* GitHub,
to find the pull requests that already exist.
The plan it prints is the one a run without `--dry-run` executes.

Three things it does not cover.
A configured `--bookmark-command` runs under `--dry-run` too, during selection,
both for `--new-command` marks and for TUI rows cycled to `[*]custom`.
`--dry-run` returns before the execute phase,
so it does not preview the *removal* of stack artifacts that `--stack-placement none` performs.
For the same reason it does not preview the server-side stack changes of `--native-stacks`:
the reconciliation under `on`/`auto`, which can dissolve and recreate existing stacks on GitHub,
and the retirement `none` performs.

## Diagnostics

Selection failures carry machine-readable codes.
None of them leaves the repository in a modified state — selection happens before anything is created or pushed.

| Code | Meaning |
|------|---------|
| `stakk::selection::rev_unresolvable` | jj rejected the revset — unknown symbol, ambiguous id prefix, syntax error; the message carries jj's diagnosis |
| `stakk::selection::rev_not_found` | The revset is valid but selects no commit |
| `stakk::selection::rev_not_unique` | The revset selects more than one commit; a PR boundary is one commit |
| `stakk::selection::rev_not_on_stack` | The commit exists but is on no submittable stack: trunk, immutable, revset-excluded, or an empty `@` (try `@-`) |
| `stakk::selection::rev_immutable` | The commit is jj-immutable and cannot take a bookmark — see below |
| `stakk::selection::empty_rev` | A selection flag was given an empty `REV`, e.g. from a shell expansion that produced nothing |
| `stakk::selection::invalid_new_spec` | A `--new` value is neither `REV` nor `REV=NAME` |
| `stakk::selection::not_colinear` | The marks do not lie on a single trunk-to-tip path; each entry in `stacks[]` is one such path |
| `stakk::selection::keep_not_found` | No such bookmark on the selected path; the names are in each segment's `bookmarks[]` |
| `stakk::selection::no_stacks` | The repository has no bookmark stacks to select from |
| `stakk::selection::duplicate_mark` | The same revision was marked twice; one boundary takes one mark, however many bookmarks it carries |
| `stakk::selection::duplicate_name` | Two marks would create the same bookmark name |
| `stakk::selection::name_exists` | The requested name is already taken by a local bookmark |
| `stakk::selection::bookmark_command_not_configured` | `--new-command` was used without `--bookmark-command` |

An empty selection is not one of these: with no marks at all, `stakk submit` means the TUI,
which without a terminal fails as `stakk::not_interactive` and exits `1`.

## Immutable commits

Commits jj considers immutable cannot take a new bookmark.
The default bookmarks revset excludes `immutable()`,
so a bookmark created there would be filtered out on every subsequent run —
stakk would create a pull request it could never see or manage again.
`--new <rev>` on such a commit fails with `stakk::selection::rev_immutable`,
and `stakk graph` reports them as `is_immutable`.

There are two ways out, and they are repository-owner decisions rather than stakk configuration:
move the work onto mutable commits, or create the bookmark manually
and drop the `~ immutable()` term from `--bookmarks-revset`.

## What `stakk graph` reports

Enough here to act on; `stakk docs graph` has the field-by-field reference.

`--format=json` is the **sparse** projection — identifiers, titles and states,
which is everything the selection flags need.
`--format=json-full` adds `commit_id`, `description`, `author` and `files[]` to every commit,
which is what reading commit messages or seeing which files changed requires — for a pull request description, say,
or a bookmark name derived from the work itself.
Sparse is a strict subset of full, so paths never change between them.

The document:

```text
schema_version, default_branch, excluded_bookmarks[], excluded_head_count
remotes[]        name, url, github ("owner/repo" | null)
stacks[]
  segments[]     bookmarks[] {name, remote_state}
    commits[]    (oldest first)
```

Per commit, in both projections: `change_id`, `short_change_id`, `title`, `committer_timestamp`, `is_immutable`,
`local_bookmark_names[]`, `is_boundary`, `is_leaf`.

Three properties of the document that are easy to trip on:

**The order of `stacks[]` is not part of the contract.**
It tracks commit recency and shifts as soon as anyone commits.
A stack has no name of its own either, and its tip need not carry a bookmark,
so `committer_timestamp` is what identifies "the most recently modified stack":

```console
stakk graph --format=json | jq '
  [.stacks[] | {
     stack: .,
     touched: ([.segments[].commits[].committer_timestamp] | max)
   }] | max_by(.touched) | .stack'
```

**`committer_timestamp` is offset-aware** (`2026-02-19T19:47:54+01:00`),
so comparing instants and comparing strings disagree: as text that value sorts after `2026-02-19T19:00:00Z`,
while being twelve minutes earlier.
The `jq` above is string-comparing, which is fine only for a repository whose commits share one offset;
`stakk docs scripting` has a version that parses.

**`title` is empty when a commit has no description.**
That is an ordinary jj state rather than an error — `stakk graph`'s pretty format writes `(no description set)`,
but the document reports the empty string.

## Reading this later

```console
stakk docs agents >> AGENTS.md
```

Redirected, `stakk docs` emits the source verbatim, so that reproduces this file byte for byte.
At a terminal it re-flows the prose to fit instead.
