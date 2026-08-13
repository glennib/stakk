# Scripting stakk

Driving `stakk` from a program: exit codes, the properties that matter to automation, and a worked example.

Two other topics carry the rest.
`stakk docs agents` has the submission model — the selection flags,
the rules that decide which commits become pull requests, and the diagnostic codes.
`stakk docs show` has the JSON schema field by field.

## Properties worth knowing

**`stacks[]` has no stable order.**
It tracks commit recency and shifts as soon as anyone commits,
so an index into it means something different on the next run.
Matching a bookmark name or comparing `committer_timestamp` does not.

**A stack need not carry a bookmark.**
A branch whose tip was never bookmarked still appears, with an empty `bookmarks[]` on its last segment,
so a name is not available as an identifier for every stack.

**`committer_timestamp` is offset-aware.** `2026-02-19T19:47:54+01:00` sorts after `2026-02-19T19:00:00Z` as a string
while being twelve minutes earlier as an instant, so the two comparisons disagree across offsets.

**`short_change_id` is unique only right now.**
It is jj's shortest unique prefix at the moment of the query, often one or two characters.
A value stored, passed between processes,
or computed well before use can resolve to `stakk::selection::rev_ambiguous` later,
where the full `change_id` still resolves.

**stakk's exit code carries the outcome.**
A wrapper that discards it turns a stopped submission into a silent one.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. `--help` and `--version` also exit `0` |
| `1` | stakk failed; the diagnostic, with its `stakk::…` code, is on stderr |
| `2` | Usage error — unknown flag, unknown subcommand, invalid enum value |
| `130` | Interrupted (`Ctrl-C` in the TUI) |

`2` comes from clap, stakk's argument parser, rather than from stakk itself,
so it follows clap's usage-error convention rather than stakk's stability contract.

Note that `stakk submit` with no selection flags means the TUI.
Without a terminal that is `stakk::not_interactive` and exit `1`,
which is what an empty selection looks like when a shell substitution expands to nothing.

## A worked example

Submits the most recently modified stack, keeping the bookmarks it already has and auto-naming an unbookmarked tip.
Requires Python 3.11 or newer for `datetime.fromisoformat` to accept jj's offsets and `Z` suffix.

```python
#!/usr/bin/env python3
"""Submit the most recently modified stack, without the TUI.

Usage: submit-stack.py [--dry-run]
"""

import json
import subprocess
import sys
from datetime import datetime


def show_json() -> dict:
    """stakk show is offline: it queries jj only, never GitHub."""
    proc = subprocess.run(
        ["stakk", "show", "--format=json"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(proc.returncode)
    return json.loads(proc.stdout)


def last_touched(stack: dict) -> datetime:
    """When this stack was last modified.

    Parsed rather than string-compared: jj emits offset-aware
    timestamps, so "2026-02-19T19:47:54+01:00" sorts after
    "2026-02-19T19:00:00Z" as text while being twelve minutes
    earlier as an instant.
    """
    return max(
        datetime.fromisoformat(commit["committer_timestamp"])
        for segment in stack["segments"]
        for commit in segment["commits"]
    )


def latest_stack(stacks: list) -> dict:
    """The most recently modified stack.

    Chosen explicitly rather than by taking stacks[0]: the document's
    order is not part of the contract, and a stack need not carry a
    bookmark to be identified by name.
    """
    if not stacks:
        sys.exit("no bookmark stacks in this repository")
    return max(stacks, key=last_touched)


def selection_flags(stack: dict) -> list:
    """One mark per segment: every PR boundary is named explicitly."""
    flags = []
    for segment in stack["segments"]:
        if segment["bookmarks"]:
            # One mark per segment, not per name. Two marks on one
            # commit is stakk::selection::duplicate_mark.
            flags.append(f"--keep={segment['bookmarks'][0]['name']}")
            continue
        # The unbookmarked head, always last when present. Unmarked,
        # it sits above the topmost boundary and is not submitted.
        tip = segment["commits"][-1]
        if tip["is_immutable"]:
            print(
                f"tip {tip['short_change_id']} is immutable, skipping",
                file=sys.stderr,
            )
            continue
        # change_id, not short_change_id: short prefixes are unique
        # only against the repository as it stands right now.
        flags.append(f"--new-auto={tip['change_id']}")
    return flags


def describe(stack: dict) -> None:
    """Report what a submission would touch, before touching it."""
    for segment in stack["segments"]:
        for bookmark in segment["bookmarks"]:
            print(f"  {bookmark['name']}: {bookmark['remote_state']}")


def submit(flags: list, dry_run: bool) -> None:
    """Run stakk submit, streaming output, and adopt its exit code."""
    argv = ["stakk", "submit", *flags]
    if dry_run:
        argv.append("--dry-run")
    # The child inherits this stdout. Python block-buffers when stdout is
    # a pipe, so without the flush our own lines land after stakk's.
    sys.stdout.flush()
    proc = subprocess.run(argv)
    if proc.returncode != 0:
        sys.exit(proc.returncode)


def main() -> None:
    dry_run = "--dry-run" in sys.argv[1:]

    stack = latest_stack(show_json()["stacks"])
    describe(stack)

    flags = selection_flags(stack)
    if not flags:
        # Bare `stakk submit` means the TUI, or exit 1 with no tty.
        sys.exit("no PR boundaries found; nothing to submit")

    submit(flags, dry_run=True)
    if not dry_run:
        submit(flags, dry_run=False)


if __name__ == "__main__":
    main()
```

`describe` is there to show what `remote_state` makes possible: it comes out of `stakk show` without touching GitHub,
so a script can report what a submission would do — "two already pushed, one new" — before running it.

## The same thing in shell

For the common case where every segment is already bookmarked:

```console
stakk submit $(stakk show --format=json \
  | jq -r '.stacks[0].segments[]
           | select(.bookmarks | length > 0)
           | "--keep=\(.bookmarks[0].name)"')
```

This indexes `.stacks[0]`, so it picks whichever stack is currently newest rather than a chosen one,
and a segment with no bookmark is filtered out rather than reported — an unbookmarked tip is silently left unsubmitted.
Both are fine for a one-off in a single-stack repository, and both are why the Python version is longer.
