<!--- stakk-docs
summary: Driving stakk from a program: exit codes and a worked example.
--->

# Scripting stakk

Exit codes and a worked example.
The submission model and diagnostic codes are in `stakk docs agents`, the JSON schema in `stakk docs graph`,
and what a program may rely on across releases in `stakk docs stability`.

## Properties that matter to a program

- **`stacks[]` has no stable order** and a stack need not carry a bookmark.
  Select by bookmark name or by `committer_timestamp`, never by index.
- **`committer_timestamp` is offset-aware.**
  `2026-02-19T19:47:54+01:00` sorts after `2026-02-19T19:00:00Z` as a string while being earlier as an instant.
- **`short_change_id` is unique only right now.**
  Store and pass `change_id`; the short form can later fail as `stakk::selection::rev_unresolvable`.
- **The exit code carries the outcome.**
  A wrapper that discards it turns a stopped submission into a silent one.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (`--help` and `--version` too) |
| `1` | stakk failed; the diagnostic with its `stakk::…` code is on stderr |
| `2` | Usage error — unknown flag, subcommand or enum value (clap's convention, not stakk's contract) |
| `130` | Interrupted (`Ctrl-C` in the TUI) |

`stakk submit` with no selection flags means the TUI; without a terminal that is `stakk::not_interactive`, exit `1`.
That is what an empty selection looks like when a shell substitution expands to nothing.

## A worked example

Submits the most recently modified stack, keeping its bookmarks and auto-naming an unbookmarked tip.
Python 3.11 or newer, for `datetime.fromisoformat` to accept jj's offsets.

```python
#!/usr/bin/env python3
"""Submit the most recently modified stack, without the TUI.

Usage: submit-stack.py [--dry-run]
"""

import json
import subprocess
import sys
from datetime import datetime


def graph_json() -> dict:
    """stakk graph is offline: jj only, never GitHub."""
    proc = subprocess.run(
        ["stakk", "graph", "--format=json"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(proc.returncode)
    return json.loads(proc.stdout)


def last_touched(stack: dict) -> datetime:
    """Parsed, not string-compared: the timestamps are offset-aware."""
    return max(
        datetime.fromisoformat(commit["committer_timestamp"])
        for segment in stack["segments"]
        for commit in segment["commits"]
    )


def latest_stack(stacks: list) -> dict:
    """Chosen by content: the order of stacks[] is not a contract."""
    if not stacks:
        sys.exit("no bookmark stacks in this repository")
    return max(stacks, key=last_touched)


def selection_flags(stack: dict) -> list:
    """One mark per segment: every PR boundary is named explicitly."""
    flags = []
    for segment in stack["segments"]:
        if segment["bookmarks"]:
            # One mark per segment, not per name: two marks on one
            # commit is stakk::selection::duplicate_mark.
            flags.append(f"--keep={segment['bookmarks'][0]['name']}")
            continue
        # The unbookmarked head, always last when present. Unmarked,
        # it is above the topmost boundary and not submitted.
        tip = segment["commits"][-1]
        if tip["is_immutable"]:
            print(
                f"tip {tip['short_change_id']} is immutable, skipping",
                file=sys.stderr,
            )
            continue
        # change_id, not short_change_id: the short form is unique
        # only against the repository as it stands right now.
        flags.append(f"--new-auto={tip['change_id']}")
    return flags


def describe(stack: dict) -> None:
    """What a submission would push, known before touching GitHub."""
    for segment in stack["segments"]:
        for bookmark in segment["bookmarks"]:
            print(f"  {bookmark['name']}: {bookmark['remote_state']}")


def submit(flags: list, dry_run: bool) -> None:
    """Run stakk submit, streaming output, and adopt its exit code."""
    argv = ["stakk", "submit", *flags]
    if dry_run:
        argv.append("--dry-run")
    # The child inherits stdout; flush so our lines land before its.
    sys.stdout.flush()
    proc = subprocess.run(argv)
    if proc.returncode != 0:
        sys.exit(proc.returncode)


def main() -> None:
    dry_run = "--dry-run" in sys.argv[1:]

    stack = latest_stack(graph_json()["stacks"])
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

## The same thing in shell

When every segment is already bookmarked and the repository has one stack:

```console
stakk submit $(stakk graph --format=json \
  | jq -r '.stacks[0].segments[]
           | select(.bookmarks | length > 0)
           | "--keep=\(.bookmarks[0].name)"')
```

This takes whichever stack is currently newest and silently leaves an unbookmarked tip unsubmitted —
the two shortcuts the Python version avoids.
