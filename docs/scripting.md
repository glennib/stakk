# Non-interactive submission

How to drive stakk from a script or a coding agent, with no TUI and no terminal.

Submitting a stack without the TUI is a two-command loop — discover, then submit.
Everything `stakk submit` needs (change id prefixes, bookmark names) comes out of `stakk show`.

```console
stakk show --format=json
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk --dry-run
stakk submit --keep base --new qzvs=my-feature --new-auto wmtk
```

`stakk show` is fully offline — it queries only `jj`, never GitHub — so discovery is cheap and safe to run at any time.

## Always name the subcommand

Write `stakk submit`, and put its flags after it.
`stakk submit --dry-run` works, while `stakk --dry-run submit` is rejected as an unexpected argument.
Bare `stakk` with no arguments is the exception — it means `stakk submit`, which opens the TUI.

The two global flags, `--config` and `--github-host`, are free of that rule: they belong to every subcommand,
so `stakk --github-host X submit` and `stakk submit --github-host X` are the same.

## Selection flags

The selection flags replace the TUI with a fully explicit selection.
They are deliberately CLI-only: no environment variables and no config keys,
because a persisted default would silently change what gets submitted.

| Flag | Meaning |
|------|---------|
| `--keep <bookmark>` | Keep an existing bookmark as a PR boundary (repeatable) |
| `--new <rev>[=<name>]` | New bookmark at `rev` — `stakk-<change_id>` by default, or `name` (repeatable) |
| `--new-auto <rev>` | New TF-IDF-named bookmark at `rev`, honoring `--auto-prefix` (repeatable) |
| `--new-command <rev>` | New bookmark at `rev` named by `--bookmark-command` (repeatable) |

`rev` is a change id or commit id prefix, exactly as printed by `stakk show`.

## Rules

**The marks fully determine the PR set — nothing is implicit.**
A commit only becomes a PR boundary if you mark it.

**All marks must lie on one trunk-to-tip path**, and the topmost mark is the tip of the submission.
Marks on diverging branches are rejected with `stakk::selection::not_colinear`.

**Bookmarks on the path that are not kept fold into the PR above them.**
This means the minimal selection produces the *fewest* PRs, not the most —
an unmarked commit is absorbed into the next boundary, it does not silently get a PR of its own.

**`--new-auto` falls back** to `stakk-<change_id>` when no name can be derived from the commit,
or when the derived name is already taken.

## Submitting a whole stack

There is no bulk flag: every PR boundary is named on the command line.
Keeping the boundaries a stack already has means enumerating them, which `stakk show` can do for you:

```console
stakk submit $(stakk show --format=json \
  | jq -r '.stacks[0].segments[]
           | select(.bookmark_names | length > 0)
           | "--keep=\(.bookmark_names[0])"')
```

`.stacks[0]` is the first stack in the document; index another one, or filter on a bookmark name,
when the repository has several.

One `--keep` per *segment*, not per bookmark name.
A commit can carry several bookmarks,
and two `--keep`s naming bookmarks on the same commit are two marks on one boundary —
`stakk::selection::duplicate_mark`.
Taking `bookmark_names[0]` picks one name per boundary; name a different one explicitly when the choice matters.
Segments with no bookmark are skipped: there is nothing to keep, and they fold into the boundary above.

## Dry runs

`--dry-run` is fully inert.
It prints the planned bookmark creations and PR actions without creating bookmarks, pushing, or touching GitHub —
so it is always safe to run first and diff against what you expected.

Note that `--dry-run` returns before the execute phase,
so it does not preview the *removal* of stack artifacts that `--stack-placement none` would perform.

## Errors

Selection failures carry machine-readable diagnostic codes and point back at `stakk show`:

| Code | Meaning |
|------|---------|
| `stakk::selection::rev_not_found` | No commit on the graph matches the prefix |
| `stakk::selection::rev_ambiguous` | The prefix matches more than one commit |
| `stakk::selection::rev_immutable` | The commit is jj-immutable and cannot take a bookmark |
| `stakk::selection::empty_rev` | A selection flag was given an empty `REV` |
| `stakk::selection::invalid_new_spec` | A `--new` value is neither `REV` nor `REV=NAME` |
| `stakk::selection::not_colinear` | The marks do not lie on a single trunk-to-tip path |
| `stakk::selection::keep_not_found` | No such bookmark on the selected path |
| `stakk::selection::no_stacks` | The repository has no bookmark stacks to select from |
| `stakk::selection::duplicate_mark` | The same revision was marked twice |
| `stakk::selection::duplicate_name` | Two marks would create the same bookmark name |
| `stakk::selection::name_exists` | The requested name is already taken by a local bookmark |
| `stakk::selection::bookmark_command_not_configured` | `--new-command` was used without `--bookmark-command` |

## The `stakk show --format=json` document

`--format=json` emits a schema-versioned document for machine consumption.
Identifiers in it — change id prefixes and bookmark names — can be passed straight to `stakk submit`.

There are two projections of one schema.
`--format=json` is **sparse**: enough to pinpoint a segment and drive `stakk submit`, nothing more.
`--format=json-full` is the **full** document.
Sparse is a strict subset of full — every sparse field appears in full under the same name,
with the same type and the same value — so a script can move from one to the other without rewriting its paths.
Both report the same `schema_version`.

Sparse is the discovery format because commit bodies, author blocks and `files[]` dominate the byte count:
dropping them shrinks the document by close to an order of magnitude on a repository like this one,
and the gap widens with commit-message length.
Reach for `json-full` when you actually need to read commit messages or see which files a commit touches.

Both projections carry the same top level:

- `schema_version` — currently `2`; bumped on breaking schema changes
- `default_branch`
- `remotes[]` — `name`, `url`, `github` (`owner/repo`, or `null` for non-GitHub remotes)
- `excluded_bookmark_count` — bookmarks excluded due to merge commits
- `stacks[]` — one per leaf, trunk-to-leaf; shared ancestor segments are repeated in every stack that contains them
  - `segments[]` — `bookmark_names[]` and `commits[]` (oldest first)

They differ only in the fields of each commit.
Sparse carries:

- `change_id`, `short_change_id` — identifiers; either can be passed to `stakk submit`'s selection flags.
  `short_change_id` is jj's shortest prefix that is unique *right now*, often only one or two characters,
  so it can stop being unique as the repository grows — pass `change_id` when the value is stored
  rather than used immediately, or the later run may fail with `stakk::selection::rev_ambiguous`
- `title` — the first line of the commit message
- `is_immutable`
- `local_bookmark_names[]` — unfiltered; includes bookmarks the bookmarks revset excluded
- `is_boundary` — the commit its segment's bookmarks point at
- `is_leaf` — the tip of its stack

`--format=json-full` adds, on every commit:

- `commit_id`
- `description` — the full commit message, `title` line included
- `author` — `name`, `email`, `timestamp`
- `files[]`

These four are *absent* from sparse, not null.
A script that read `.description`, `.author`,
`.files` or `.commit_id` from `--format=json` moves to `--format=json-full`.

Pulling the identifiers you need out of the first stack — all sparse fields, so plain `--format=json` suffices:

```console
stakk show --format=json \
  | jq '.stacks[0].segments[].commits[]
        | {short_change_id, local_bookmark_names}'
```

## Immutable commits

Commits jj considers immutable cannot take a new bookmark: the default bookmarks revset excludes `immutable()`,
so stakk would create a PR it could never see again on the next run.
`--new <rev>` on such a commit fails with `stakk::selection::rev_immutable`,
and the commits are annotated in `stakk show`.

If you genuinely need a PR there, create the bookmark yourself
and drop the `~ immutable()` term from `--bookmarks-revset`.
Otherwise, move the work onto mutable commits.
