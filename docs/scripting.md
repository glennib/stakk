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

## Selection flags

The selection flags replace the TUI with a fully explicit selection.
They conflict with the positional bookmark argument, and they are deliberately CLI-only:
no environment variables and no config keys, because a persisted default would silently change what gets submitted.

| Flag | Meaning |
|------|---------|
| `--keep <bookmark>` | Keep an existing bookmark as a PR boundary (repeatable) |
| `--keep-all` | Keep every existing bookmark on the selected path |
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

**Bare `--keep-all` requires the choice of stack to be unambiguous** — a single stack,
or several that agree on their bookmarks (differing only in unbookmarked heads such as the working copy).
Anchor it with `--keep`/`--new` otherwise.
Commits already carrying an explicit mark are skipped by `--keep-all`: explicit beats bulk.

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
| `stakk::selection::not_colinear` | The marks do not lie on a single trunk-to-tip path |
| `stakk::selection::keep_not_found` | No such bookmark on the selected path |
| `stakk::selection::keep_all_ambiguous` | Bare `--keep-all` cannot pick between stacks |
| `stakk::selection::no_marks` | No marks given, so there is nothing to submit |
| `stakk::selection::duplicate_mark` | The same revision was marked twice |
| `stakk::selection::duplicate_name` | Two marks would create the same bookmark name |
| `stakk::selection::name_exists` | The requested name is already taken by a local bookmark |

## The `stakk show --format=json` document

`--format=json` emits a schema-versioned document for machine consumption.
Identifiers in it — change id prefixes and bookmark names — can be passed straight to `stakk submit`.

- `schema_version` — currently `1`; bumped on breaking schema changes
- `default_branch`
- `remotes[]` — `name`, `url`, `github` (`owner/repo`, or `null` for non-GitHub remotes)
- `excluded_bookmark_count` — bookmarks excluded due to merge commits
- `stacks[]` — one per leaf, trunk-to-leaf; shared ancestor segments are repeated in every stack that contains them
  - `segments[]` — `bookmark_names[]` and `commits[]` (oldest first)
    - each commit: `change_id`, `short_change_id`, `commit_id`, `description` (full), `author` (`name`, `email`,
      `timestamp`), `files[]`, `is_immutable`, `local_bookmark_names[]` (unfiltered — includes bookmarks the
      bookmarks revset excluded), `is_boundary` (the commit its segment's bookmarks point at), `is_leaf` (the tip of
      its stack)

Pulling the identifiers you need out of the first stack:

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
