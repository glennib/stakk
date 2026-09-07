<!--- stakk-docs
summary: The `stakk graph` document, field by field.
--->

# The `stakk graph` document

`stakk graph` renders the change graph: `--format=pretty` (default) for a human,
`--format=json` and `--format=json-full` as a schema-versioned document for a machine.
It is offline — jj only — so it needs no network or credentials.

Nothing in it has been near GitHub: no PR numbers, titles, review or CI state.
`remote_state` (below) says where a *bookmark* stands against its remote, which is not the same thing;
stakk learns which PRs exist during `stakk submit`'s plan phase.

## Two projections, one schema

`--format=json` is **sparse**: identifiers, titles and states — enough to drive `stakk submit`.
`--format=json-full` adds commit bodies, authors and files, which dominate the byte count.
Sparse is a strict subset of full (same names, types, values), and both report the same `schema_version`.

## Top level

- `schema_version` — currently `2`; bumped on breaking schema changes
- `default_branch`
- `remotes[]` — `name`, `url`, `github` (`owner/repo`, or `null` for a non-GitHub remote)
- `excluded_bookmarks[]` — bookmarks left out because their history contains a merge, which the stacking model
  does not represent; stakk cannot manage these
- `excluded_head_count` — unbookmarked heads left out for the same reason, counted because they have no name
- `stacks[]` — one per leaf

## Stacks

One entry per leaf, segments trunk-to-leaf.
Shared ancestor segments are repeated in full in every stack containing them, so each entry is self-contained.

**The order is not part of the contract.**
It follows commit recency, so `stacks[0]` changes whenever anyone commits, and it is not the TUI's leaf numbering.
Identify a stack by its content; `committer_timestamp` is in the sparse projection
so "the most recently modified stack" can be computed rather than assumed.

## Segments

A segment is a run of commits ending at a PR boundary.

- `bookmarks[]` — `name` and `remote_state` per bookmark on the boundary commit; empty for an unbookmarked head
- `commits[]` — oldest first, matching the `--bookmark-command` payload

| `remote_state` | Meaning |
|----------------|---------|
| `unpushed` | No remote bookmark of this name on this commit, on any remote. A push creates it |
| `diverged` | A tracked remote disagrees with the local bookmark — the usual state after a rebase or amend. A push moves it |
| `synced` | A remote bookmark of this name sits on this commit, on *some* remote |

**`remote_state` does not know which remote you push to.**
`stakk graph` takes no `--remote`, so with several remotes a bookmark on `mirror`
but never pushed to `origin` reports `synced`.
With one remote, `synced` does mean a push is a no-op. jj's internal `name@git` never counts.

Several bookmarks on one commit are one boundary: two `--keep`s naming them is `stakk::selection::duplicate_mark`.

## Commits

In both projections:

- `change_id`, `short_change_id` — either works in the selection flags.
  The short form is jj's shortest prefix unique *right now*, so store `change_id`,
  or a later run may fail with `stakk::selection::rev_unresolvable`
- `title` — first line of the commit message; empty when there is no description
  (`(no description set)` belongs to the pretty renderer, never to the document)
- `committer_timestamp` — offset-aware, e.g. `2026-02-19T19:47:54+01:00`; the key stack order is derived from.
  Compare instants, not strings: that value sorts after `2026-02-19T19:00:00Z` as text
  while being twelve minutes earlier
- `is_immutable` — cannot take a new bookmark
- `local_bookmark_names[]` — unfiltered; includes bookmarks the bookmarks revset excluded,
  unlike the segment's `bookmarks[]`
- `is_boundary` — the newest commit of its segment
- `is_leaf` — the tip of its stack

`--format=json-full` adds `commit_id`, `description` (the full message, `title` line included), `author`
(`name`, `email`, `timestamp`) and `files[]`.
They are *absent* from sparse, not null.
Stack order follows `committer_timestamp`, not `author.timestamp`, because a rebase updates the former.

## Stability

Field names, types and the sparse/full subset relationship are covered by `schema_version`.
The order of `stacks[]` and the pretty format are not.
