<!--- stakk-docs
summary: What semantic versioning covers, and what may change in any release.
--->

# Stability

stakk follows semantic versioning.
This document is the contract: what a script, an agent or another tool may rely on, and what may move under it.

The *wording* of this document is not itself stable — it is a `stakk docs` topic,
and those may be rewritten at any time.
What it describes is; the contract changes in a release that says so.

## Stable

Changing any of these needs a major release.

- **Subcommand names, their aliases, and their flags, long and short forms alike.**
  Removing an alias is as breaking as removing a flag.
- **`STAKK_`-prefixed environment variables.**
- **Config file keys and their defaults.** The list: `stakk docs config`.
- **The `stakk graph` JSON document under its `schema_version`** — field names, types and meanings.
  Currently `2`, reported by both the sparse `json` and the `json-full` projection;
  sparse stays a strict subset of full.
  The *order* of `stacks[]` is not part of this.
  Field by field: `stakk docs graph`.
- **The JSON handed to `--bookmark-command` on stdin**, under its own `schema_version` (currently `1`).
  The schema is in `stakk submit --help`.
- **Exit codes:** `0` success, `1` failure, `130` interrupted.
  `2` is clap's usage-error convention and follows clap, not this contract.
  Details: `stakk docs scripting`.

## Deprecated

Still supported and still covered by the rules above until the major release that removes them.

- **`stakk show`** — an alias for `stakk graph`, to be removed in the next major release.
  The command renders the change graph and `show` says nothing about that
  (in jj's vocabulary `show` is a single commit).
  Migration is the command name only; flags, output and `schema_version` are unchanged.

## Not stable

These may change in any release.

- **The rendered text of `stakk docs` and `--help`.**
  Only the `stakk docs <topic>` invocation shape is stable.
- **The order of `stacks[]` in the `stakk graph` JSON.**
  Choose a stack by its bookmarks or contents, never by index.
- **The `pretty` output of `stakk graph`.**
  `--format=json` is the one for a program.
- **The TUI layout and keybindings.**
- **Spinner and progress text.**
- **Diagnostic codes (`stakk::…`) and error wording.**
  A code is still what a program should match on; prose may change silently, and a code that is added, split,
  merged or renamed is listed in the changelog.
  Treat an unknown code as a plain failure — the exit code is the signal.
- **Advisory warnings on stderr.**
  They come and go; stderr output is not failure, the exit code is.
- **The server-side behaviour behind `--native-stacks`.**
  The flag and its values are stable surface, but GitHub's stacked pull requests are a public preview,
  so what a registered stack looks like and when GitHub retargets or rebases can move without a stakk release,
  and stakk may change how it converges the stack (create, append, dissolve-and-recreate) in any release.

## Not a breaking change

**Raising the minimum supported jj version.**
The check is warn-only, so the floor can move in any release — normally the one that adopts a newer jj's behaviour.
