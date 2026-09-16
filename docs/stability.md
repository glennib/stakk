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
- **`STAKK_`-prefixed environment variables**, and `FORGEJO_TOKEN`.
  The GitHub token variables (`GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`, `GH_HOST`)
  follow the GitHub CLI's names and are stable for as long as it keeps them.
- **Config file keys and their defaults.** The list: `stakk docs config`.
  One default is exempt: `native_stacks` is `ignore` while GitHub's stacked pull requests are a public preview,
  and may become `auto` in a *minor* release once they leave it.
  The release that flips it says so in the changelog; nothing else about the key changes.
- **The `stakk graph` JSON document under its `schema_version`** — field names, types and meanings.
  Currently `3`, reported by both the sparse `json` and the `json-full` projection;
  sparse stays a strict subset of full.
  The *order* of `stacks[]` is not part of this.
  Field by field: `stakk docs graph`.
- **The render context of `--template-path` templates.**
  The top-level names `stack`, `stack_size`, `default_branch`, `current_bookmark` and `stakk_url`;
  on each `stack` entry `bookmark_name`, `pr_url`, `pr_number`, `title`, `base`, `is_draft`, `position`,
  `is_current` and `is_leaf`; and the trunk-first order of `stack`.
  A name may be added in any release; renaming or removing one, or reordering `stack`, needs a major.
  The built-in template's text is not covered — see below.
- **The JSON handed to `--bookmark-command` on stdin**, under its own `schema_version` (currently `1`).
  The schema is in `stakk submit --help`.
- **Exit codes:** `0` success, `1` failure, `130` interrupted.
  `2` is clap's usage-error convention and follows clap, not this contract.
  Details: `stakk docs scripting`.

## Not stable

These may change in any release.

- **The rendered text of `stakk docs` and `--help`.**
  Only the `stakk docs <topic>` invocation shape is stable.
- **The built-in stack comment template.**
  What it renders may change in any release; the context it renders from is stable (above).
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
