# Stability

stakk follows semantic versioning.
This document is the contract: the surface a script, an agent, or another tool may rely on,
and the surface that may move under it without notice.

The *wording* of this document is not itself stable — it is a `stakk docs` topic,
and those are free to be reworded or restructured at any time.
What it describes is.
The contract changes when the contract changes, in a release that says so.

## Stable

Changing any of these needs a major release.

- **Subcommand names and their flags.**
- **`STAKK_`-prefixed environment variables.**
- **Config file keys and their defaults.**
  Precedence and the full key list: `stakk docs config`.
- **The `stakk show` JSON document, under its `schema_version`** — field names, types and meanings.
  It is currently `2`, and both the sparse `json` and the `json-full` projection report it.
  Sparse stays a strict subset of full.
  The *order* of `stacks[]` is not part of this.
  Field by field: `stakk docs show`.
- **The JSON handed to `--bookmark-command` on stdin**, under its own `schema_version` (currently `1`).
  The schema, with a worked example, is in `stakk submit --help`.
- **Diagnostic codes (`stakk::…`).**
  The prose of an error may be rewritten in any release; the code identifying it may not.
  A program should match on the code.
- **Exit codes:** `0` success, `1` failure, `130` interrupted.
  `2` is clap's usage-error convention and follows clap, not this contract.
  The table, and the rules a program should follow around it: `stakk docs scripting`.

## Not stable

These may change in any release.

- **The rendered text of `stakk docs` and `--help`.**
  Topics may be added, reworded, or restructured at any time; only the `stakk docs <topic>` invocation shape is stable.
- **The order of `stacks[]` in the `stakk show` JSON.**
  It tracks commit recency, so it moves as you commit anywhere in the repository,
  and it is computed separately from the TUI's leaf numbering.
  Choose a stack by its bookmark names or its contents, never by index.
- **The `pretty` output of `stakk show`.**
  It is drawn for a human; `--format=json` is the one for a program.
- **The TUI layout and keybindings.**
- **Spinner and progress text.**
- **Error message wording** — the diagnostic codes are the contract, not the prose.
- **Advisory warnings printed to stderr.**
  They may be added or removed in any release,
  so a program must not depend on one appearing and must not treat stderr output as failure.
  The exit code is the signal.

## Not a breaking change

**Raising the minimum supported jj version.**
The check is warn-only — stakk runs against an older jj and says so — so the floor can move in any release,
normally the one that actually adopts a newer jj's behaviour.
