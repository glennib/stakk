<!--- stakk-docs
summary: The `stakk graph` document, field by field.
--->

# The `stakk graph` document

`stakk graph` renders the change graph.
`--format=pretty` (the default) draws it for a human;
`--format=json` and `--format=json-full` emit a schema-versioned document for a machine.

It is offline.
It queries `jj` and nothing else, so it is cheap, safe to run at any time, and works without network or credentials.

## What is not in it

No pull request state.
Not whether a PR exists, not its number, title, review status or CI result.
Nothing here has been near GitHub.

The closest thing is `remote_state`, which says where a *bookmark* stands against its remote — see **Segments** below.
A pushed bookmark need not have a pull request, and stakk only learns which do during `stakk submit`'s plan phase.

## Two projections, one schema

`--format=json` is **sparse**: enough to pinpoint a segment, drive `stakk submit`, and judge what a submission would do.
`--format=json-full` is the **full** document.

Sparse is a strict subset of full — every sparse field appears in full under the same name,
with the same type and the same value — so a consumer can move between them without rewriting its paths.
Both report the same `schema_version`.

Sparse is the discovery format because commit bodies, author blocks and `files[]` dominate the byte count:
dropping them shrinks the document by close to an order of magnitude on a repository like this one,
and the gap widens with commit-message length.
`json-full` is what carries commit messages and the files a commit touched.

## Top level

Both projections carry:

- `schema_version` — currently `2`; bumped on breaking schema changes
- `default_branch`
- `remotes[]` — `name`, `url`, `github` (`owner/repo`, or `null` for
  non-GitHub remotes)
- `excluded_bookmarks[]` — names of bookmarks left out of the graph because
  of merge commits in their history
- `excluded_head_count` — unbookmarked heads left out for the same reason,
  counted rather than named because they have no name to report
- `stacks[]` — one per leaf, trunk-to-leaf

A non-empty `excluded_bookmarks` means stakk cannot manage those bookmarks: their history contains a merge,
which the stacking model does not represent.

## Stacks

One entry per leaf of the graph.
Shared ancestor segments are repeated in full in every stack that contains them,
so each entry is self-contained and no joining across entries is needed.

**The order is not part of the contract.**
Stacks are sorted by commit recency, newest first, compared as instants and tie-broken deterministically —
but that is an implementation detail, it is not the TUI's leaf numbering,
and `stacks[0]` moves as soon as anyone commits anywhere in the repository.

What identifies a stack is its content.
`committer_timestamp` is in the sparse projection precisely so
that a consumer wanting "the most recently modified stack" can compute it rather than rely on a position.

## Segments

A segment is a run of commits ending at a PR boundary.

- `bookmarks[]` — `name` and `remote_state` for each bookmark on the boundary commit.
  Empty for the unbookmarked head segment
- `commits[]` — oldest first (trunk side first), matching the
  `--bookmark-command` payload convention

`remote_state` is one of:

| Value | Meaning |
|-------|---------|
| `unpushed` | No remote bookmark of this name on this commit, on any remote. A push creates it |
| `diverged` | A tracked remote disagrees with the local bookmark — the usual state after a rebase or an amend. A push moves it |
| `synced` | A remote bookmark of this name sits on this commit, on *some* remote. See the caveat below |

Two `jj` facts produce this, and neither is sufficient alone.
`jj`'s own `synced()` is false only when a *tracked* remote disagrees with the local bookmark,
so a never-pushed bookmark reports `synced = true` exactly like an up-to-date one.
What separates them is whether a remote bookmark of the same name sits on the boundary commit. jj's internal `name@git`
remote does not count — it tracks the colocated git repository, not a push target.

**`remote_state` does not know which remote you push to.** `stakk graph` takes no `--remote`,
so any remote but `name@git` satisfies the match.
In a repository with more than one remote,
a bookmark that exists on `mirror` but was never pushed to `origin` reports `synced`,
and `stakk submit --remote origin` then pushes it for the first time.
With a single remote — the ordinary case — `synced` does mean a push would be a no-op.
Read `remotes[]` when the distinction matters.

A commit can carry several bookmarks, so `bookmarks[]` can have more than one entry.
They are one boundary, not several: two `--keep`s naming two bookmarks on the same commit are two marks on one boundary,
which fails as `stakk::selection::duplicate_mark`.

## Commits

Sparse carries:

- `change_id`, `short_change_id` — identifiers; either can be passed to `stakk submit`'s selection flags.
  `short_change_id` is jj's shortest prefix that is unique *right now*, often only one or two characters,
  so it can stop being unique as the repository grows.
  Pass `change_id` when the value is stored rather than used immediately,
  or the later run may fail with `stakk::selection::rev_ambiguous`
- `title` — the first line of the commit message.
  Empty when the commit has no description, which is a normal jj state;
  the `(no description set)` wording belongs to the pretty renderer, never to the document
- `committer_timestamp` — offset-aware, e.g. `2026-02-19T19:47:54+01:00`.
  This is the key stack order is derived from,
  and it is in the sparse projection so a consumer can reproduce that order or impose its own.
  Compare instants, not strings: as text that value sorts *after* `2026-02-19T19:00:00Z`
  while being twelve minutes earlier
- `is_immutable` — jj considers the commit immutable, so it cannot take a
  new bookmark
- `local_bookmark_names[]` — unfiltered; includes bookmarks the bookmarks
  revset excluded, unlike the segment's `bookmarks[]`
- `is_boundary` — the newest commit of its segment.
  For a bookmarked segment that is the commit the bookmarks point at; an unbookmarked head segment has one too,
  pointing at nothing
- `is_leaf` — the tip of its stack

`--format=json-full` adds, on every commit:

- `commit_id`
- `description` — the full commit message, `title` line included
- `author` — `name`, `email`, `timestamp`
- `files[]`

Those four are *absent* from sparse, not null.
A consumer that reads `.description`, `.author`,
`.files` or `.commit_id` from `--format=json` moves to `--format=json-full`.

Note that `author.timestamp` and `committer_timestamp` are different fields answering different questions.
Stack order follows the committer timestamp, because that is what a rebase updates.

## Stability

Field names, types and the sparse/full subset relationship are covered by the `schema_version`.
The **order of `stacks[]` is not**, and neither is the pretty format, which is for humans and free to change.
