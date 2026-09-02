<!--- stakk-docs
summary: What semantic versioning covers, and what may change in any release.
--->

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

- **Subcommand names, their aliases, and their flags.**
  An alias is as binding as the name it stands for: removing one needs a major release, same as removing a flag.
- **`STAKK_`-prefixed environment variables.**
- **Config file keys and their defaults.**
  Precedence and the full key list: `stakk docs config`.
- **The `stakk graph` JSON document, under its `schema_version`** — field names, types and meanings.
  It is currently `2`, and both the sparse `json` and the `json-full` projection report it.
  Sparse stays a strict subset of full.
  The *order* of `stacks[]` is not part of this.
  Field by field: `stakk docs graph`.
- **The JSON handed to `--bookmark-command` on stdin**, under its own `schema_version` (currently `1`).
  The schema, with a worked example, is in `stakk submit --help`.
- **Exit codes:** `0` success, `1` failure, `130` interrupted.
  `2` is clap's usage-error convention and follows clap, not this contract.
  The table, and the rules a program should follow around it: `stakk docs scripting`.

## Deprecated

Still supported, and still covered by the rules above until the major release that removes them.
Listed here so a script has notice rather than a surprise.

- **`stakk show`** — an alias for `stakk graph`.
  The command builds and renders the change graph, and `show` says nothing about that;
  in jj's own vocabulary `show` is a single commit, which is the opposite of what this prints.
  The alias works exactly as `stakk graph` does and will be removed in the next major release.
  Migration is the command name and nothing else: flags, output, `--format` values and `schema_version` are unchanged.

## Not stable

These may change in any release.

- **The rendered text of `stakk docs` and `--help`.**
  Topics may be added, reworded, or restructured at any time; only the `stakk docs <topic>` invocation shape is stable.
- **The order of `stacks[]` in the `stakk graph` JSON.**
  It tracks commit recency, so it moves as you commit anywhere in the repository,
  and it is computed separately from the TUI's leaf numbering.
  Choose a stack by its bookmark names or its contents, never by index.
- **The `pretty` output of `stakk graph`.**
  It is drawn for a human; `--format=json` is the one for a program.
- **The TUI layout and keybindings.**
- **Spinner and progress text.**
- **Diagnostic codes (`stakk::…`) and error wording.**
  A code is still what a program should match on.
  Prose may be rewritten silently; a code that is added, split, merged or renamed is listed in the changelog.
  The set of codes itself may change in any release as the error model settles.
  Treat an unknown code as a plain failure; the exit code is the signal.
- **Advisory warnings printed to stderr.**
  They may be added or removed in any release,
  so a program must not depend on one appearing and must not treat stderr output as failure.
  The exit code is the signal.
- **The server-side behavior behind `--native-stacks`.**
  The flag and its values are stable surface like any other,
  but GitHub's stacked pull requests are a public preview the vendor declares subject to change,
  so the reconciliation's observable effect on GitHub — what a registered stack looks like,
  when GitHub retargets or rebases — can move without a stakk release at all,
  and stakk may adjust how it converges the stack (create, append, dissolve-and-recreate) in any release.

## Not a breaking change

**Raising the minimum supported jj version.**
The check is warn-only — stakk runs against an older jj and says so — so the floor can move in any release,
normally the one that actually adopts a newer jj's behaviour.
