# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.4.1](https://github.com/glennib/stakk/compare/v2.4.0...v2.4.1) - 2026-09-01

### Other

- *(cli)* group flags under help headings

## [2.4.0](https://github.com/glennib/stakk/compare/v2.3.0...v2.4.0) - 2026-09-01

### Added

- *(cli)* default `--stack-placement` to `auto-comment`
- *(forge)* integrate GitHub's native stacked pull requests (`--native-stacks`)

### Other

- update gif
- *(deps)* update dependency uv to v0.12.8 ([#230](https://github.com/glennib/stakk/pull/230))

## [2.3.0](https://github.com/glennib/stakk/compare/v2.2.1...v2.3.0) - 2026-08-31

### Added

- *(select)* show short change ids in the graph screen

### Other

- *(deps)* lock file maintenance ([#229](https://github.com/glennib/stakk/pull/229))
- *(deps)* update dependency uv to v0.12.7 ([#228](https://github.com/glennib/stakk/pull/228))
- *(deps)* update dependency rumdl to v0.2.62 ([#227](https://github.com/glennib/stakk/pull/227))
- *(deps)* update dependency rumdl to v0.2.61 ([#226](https://github.com/glennib/stakk/pull/226))
- *(deps)* lock file maintenance ([#223](https://github.com/glennib/stakk/pull/223))
- *(deps)* update dependency uv to v0.12.6 ([#224](https://github.com/glennib/stakk/pull/224))

## [2.2.1](https://github.com/glennib/stakk/compare/v2.2.0...v2.2.1) - 2026-08-24

### Fixed

- cargo upgrade
- *(lint)* satisfy clippy 1.98 unused_async_trait_impl in mock runners

### Other

- *(deps)* update dependency cargo-binstall to v1.22.0 ([#221](https://github.com/glennib/stakk/pull/221))
- *(deps)* update dependency rumdl to v0.2.60 ([#220](https://github.com/glennib/stakk/pull/220))
- *(deps)* update dependency rumdl to v0.2.58 ([#219](https://github.com/glennib/stakk/pull/219))
- *(deps)* update dependency rumdl to v0.2.57 ([#218](https://github.com/glennib/stakk/pull/218))
- *(deps)* update dependency rumdl to v0.2.56 ([#216](https://github.com/glennib/stakk/pull/216))

## [2.2.0](https://github.com/glennib/stakk/compare/v2.1.3...v2.2.0) - 2026-08-17

### Added

- *(comment)* mark the top of the stack in the default template

### Other

- *(deps)* lock file maintenance ([#215](https://github.com/glennib/stakk/pull/215))
- upgrade dist

## [2.1.3](https://github.com/glennib/stakk/compare/v2.1.2...v2.1.3) - 2026-08-14

### Fixed

- *(docs)* parse the topic preamble with either line ending

### Other

- *(deps)* update dependency uv to v0.12.5 ([#213](https://github.com/glennib/stakk/pull/213))

## [2.1.2](https://github.com/glennib/stakk/compare/v2.1.1...v2.1.2) - 2026-08-14

### Other

- *(docs)* generate the `stakk docs` topics from the docs/ directory
- *(docs)* snapshot the `stakk docs` index and help output

## [2.1.1](https://github.com/glennib/stakk/compare/v2.1.0...v2.1.1) - 2026-08-14

### Fixed

- *(comment)* render stack entries as list items so GitHub expands the PR links

## [2.1.0](https://github.com/glennib/stakk/compare/v2.0.0...v2.1.0) - 2026-08-14

### Added

- *(cli)* add one-letter aliases for submit, graph and docs
- *(cli)* rename `stakk show` to `stakk graph`

### Other

- *(cli)* rename GraphArgs to RevsetArgs
- move the stability contract into a `stakk docs` topic
- improve README
- *(media)* update gif

## [2.0.0](https://github.com/glennib/stakk/compare/v1.25.0...v2.0.0) - 2026-08-13

v2 is a surface release.
Nothing about how stakk talks to jj or to GitHub changed;
what changed is what you are allowed to type, what the JSON says, and what any of it promises.

Three things drove it.
Issue [#202](https://github.com/glennib/stakk/issues/202) showed that the top-level flatten made subcommand names
shadow bookmark names and let `stakk --draft show` silently open real pull requests.
The `--format=json` document had grown into something an agent pays for on every discovery call.
And the project had never said, out loud, which parts of its surface a script may rely on.

Every worthwhile break was collected into this one release, and the result is smaller than v1 in every direction:
one parse path, one phase-1 constructor, one draft knob, one selection language in which **every PR boundary is named
on the command line**.

`stakk` and `stakk submit` still mean the TUI. If that is how you use it, nothing below applies to you.

### Migrating from v1

| v1                                            | v2                                                                                     |
| --------------------------------------------- | -------------------------------------------------------------------------------------- |
| `stakk`                                       | unchanged — TUI                                                                        |
| `stakk submit`                                | unchanged — TUI                                                                        |
| `stakk <bookmark>` / `stakk -- <bookmark>`    | `stakk submit --keep <b1> … --keep <bookmark>`                                          |
| `stakk submit <bookmark>`                     | same as above                                                                          |
| `stakk submit --keep-all`                     | enumerate the boundaries; `stakk docs scripting` has a jq idiom that generates them     |
| `stakk --dry-run …` (flags before subcommand) | `stakk submit --dry-run …`                                                             |
| `--draft` / `STAKK_DRAFT`                     | `--pr-mode draft` / `STAKK_PR_MODE=draft` / `pr_mode = "draft"`                          |
| `--template` / `STAKK_TEMPLATE` / `template`  | `--template-path` / `STAKK_TEMPLATE_PATH` / `template_path`                              |
| `stakk show --format=json` (full document)    | `stakk show --format=json-full` — sparse is the new `json`                               |
| `.segments[].bookmark_names[]`                | `.segments[].bookmarks[].name`, now with a `remote_state` beside it                      |
| `.excluded_bookmark_count`                    | `.excluded_bookmarks[]` and `.excluded_head_count`                                       |
| `GITHUB_TOKEN` beats `GH_TOKEN` (no gh)       | `GH_TOKEN` beats `GITHUB_TOKEN`, matching the GitHub CLI                                 |
| `stakk auth setup`                            | `stakk docs auth`                                                                       |
| `stakk auth test`                             | `gh auth status --hostname <host>`, or `stakk submit --dry-run` *with selection flags*   |

A stale `stakk.toml` key fails loudly — `Config` denies unknown fields.
A stale `STAKK_DRAFT` or `STAKK_TEMPLATE` in your shell profile now warns on stderr and names its replacement;
see **Added** below for why that one was worth building.

Removed diagnostic codes, for anything matching on them:
`stakk::submit::bookmark_not_found`, `stakk::selection::keep_all_ambiguous`, `stakk::selection::no_marks`.

### Breaking

- **The positional bookmark is gone.**
  No `stakk <bookmark>`, no `stakk submit <bookmark>`, no `stakk -- <bookmark>`.
  Every submission goes through selection: no flags means the TUI, and `--keep`/`--new`/`--new-auto`/`--new-command`
  spell a non-interactive one explicitly.
  Replace `stakk submit BM` with `stakk submit --keep b1 … --keep BM`, naming every bookmarked boundary from trunk
  through `BM` — the same PR set, with colinearity validated instead of the positional's silent first-stack pick.
  This ends the subcommand-shadowing constraint: subcommand and alias names are no longer a compatibility surface,
  because there is no bare form left for a bookmark name to collide with.
- **Submit flags no longer parse before a subcommand.**
  `Cli` no longer flattens `SubmitArgs` at the top level, and `args_conflicts_with_subcommands` went with it.
  `stakk --dry-run` and `stakk --dry-run show` now fail with clap's stock "unexpected argument";
  spell them `stakk submit --dry-run`.
  All three defects in #202 are gone by construction rather than by guard.
  Bare `stakk` is unchanged and still picks up `STAKK_*` variables and config-injected defaults, because its arguments
  come from a real clap parse of the synthetic argv `stakk submit` rather than a hand-built struct.
  The global flags gained placement freedom as a side effect: `--config` and `--github-host` now parse on either side of
  the subcommand, so `stakk --github-host X show` works where it used to hit the conflict error.
- **`--keep-all` is removed.**
  It was the last implicit construct in the selection language and carried the most spec debt: the bare-form ambiguity
  rule, two diagnostics that existed only to report it, and the explicit-beats-bulk skip rule.
  Submitting a whole stack non-interactively is now a `stakk show --format=json` plus `jq` idiom, documented in
  `stakk docs scripting` — one mark per *segment*, not per bookmark name.
  Removal is the safe direction for a major release: re-adding a bulk flag later, this shape or a better one, is free.
- **`--draft` and `STAKK_DRAFT` are removed.**
  The flag duplicated `--pr-mode=draft` with a special override rule — draft-ness won from whichever source set it, even
  over an explicit `--pr-mode regular` — which made `STAKK_DRAFT` impossible to override, only to unset.
  One knob remains, with ordinary CLI > env > config precedence.
- **`--template` is now `--template-path`**, `STAKK_TEMPLATE` is `STAKK_TEMPLATE_PATH`, and the `template` TOML key is
  `template_path`.
  The option takes a path; the old name read as inline template content.
  The rename frees `template` for a possible future inline-template option.
- **`stakk auth` is removed** — `auth setup` was a static wall of `println!`s and `auth test` was a connectivity probe.
  `stakk docs auth` takes over the prose and gains formatting, runtime reflow and a place in the docs index for free.
  To check a token: `gh auth status --hostname <host>`, which resolves it the same way stakk does, or
  `stakk submit --dry-run` *with selection flags* for the whole remote → token → API chain read-only.
  Token *resolution* is untouched; only the subcommand went.
- **`stakk show --format=json` is now a sparse projection**, and `--format=json-full` is the previous document.
  Commit bodies, author blocks and `files[]` dominated the document, and none of them are needed to pinpoint a segment
  and feed `stakk submit`: measured on this repository when the change landed, the full document was 30,665 bytes
  against 5,082 for the sparse projection of the same graph.
  Sparse omits `commit_id`, `description`, `author` and `files[]` per commit; it omits them rather than nulling them.
  Sparse is a strict subset of full *by construction* — one struct whose full-only fields are `Option` with
  `skip_serializing_if`, so there is no second serializer that could drift — and three tests pin the subset, the exact
  key set and the field order.
  `schema_version` is now `2` in both projections.
- **github.com token precedence follows the GitHub CLI.**
  `gh help environment` documents `GH_TOKEN` before `GITHUB_TOKEN`; stakk followed that for Enterprise hosts but
  inverted it for github.com.
  With both variables exported, `gh auth token` returned one and stakk's fallback returned the other, so the same
  environment resolved to two different tokens depending on whether gh happened to be installed.
  The change is visible only when both variables are set *and* gh is absent or unauthenticated for the host; where gh
  answers, its answer already decided.

### Added

- **A stability contract**, in the README rather than a doc topic — because doc-topic text is exactly what it declares
  unstable.
  Semver-guarded: subcommand names and flags, `STAKK_` variables, config keys and their defaults, the `stakk show` JSON
  under its `schema_version`, the `--bookmark-command` payload under its own, diagnostic codes, and exit codes.
  Not guarded: rendered `docs`/`--help` text, pretty output, TUI layout and keybindings, advisory warnings, error
  *wording* (the codes are the contract), and the order of `stacks[]`.
  Raising `MIN_SUPPORTED_JJ_VERSION` is explicitly *not* breaking — the check is warn-only — so a bump can land in the
  release that actually adopts a newer jj. It stays at 0.39.0 here; v2 changes no jj invocation.
- **A warning when a removed `STAKK_` variable is still set.**
  The two halves of a removal fail very differently: a stale TOML key errors and lists the valid ones, while a stale
  variable is simply not read.
  That is worst where it costs most — someone with `STAKK_DRAFT=1` in a shell profile upgrades and opens
  ready-for-review PRs instead of drafts, notifying reviewers, with nothing to undo.
  A warning rather than an error, because stakk cannot know a `STAKK_` variable is still meant for stakk.
  Gated to the paths that submit, so `show`, `docs` and `completions` pay nothing and their stdout stays byte-clean.
- **`stakk show` reports push state.**
  Each segment now carries `bookmarks: [{name, remote_state}]`, where `remote_state` is `unpushed`, `diverged` or
  `synced`, derived offline.
  jj's `synced()` alone cannot carry this — it is false only when a *tracked* remote disagrees, so a never-pushed
  bookmark looks identical to an up-to-date one; the tiebreaker is whether a remote bookmark of the same name sits on
  the segment's boundary commit.
  It describes what a push would do, on *some* remote, and says nothing about pull requests.
- **`committer_timestamp` on every commit, in both projections** — stack order is derived from it, and the document
  previously carried only the *author* timestamp, and only in the full projection, asking consumers to trust an order
  they had no field to reproduce or override.
- **`excluded_bookmarks[]` and `excluded_head_count`** replace `excluded_bookmark_count`, which summed two different
  things: bookmarks excluded by merge taint, and unbookmarked *heads* excluded the same way.
  Heads have no name to report, so a consumer reading the count could not say which bookmarks it had lost.
- **`title` on every commit** — the first line of the message, in both projections. `description` still carries the
  whole message.
- **A `stakk docs auth` topic**, documenting token resolution as what it actually is: delegation-first.
  stakk asks gh; its own environment read is the fallback for when gh is absent or unauthenticated, not a
  lower-priority alternative.
- The non-interactive documentation is split into three topics — `stakk docs agents`, `scripting` and `show` — with a
  worked Python example alongside the jq shortcut.

### Fixed

- **Stacks are ordered by instant, not by timestamp string.**
  `stacks[]` was sorted by lexicographic comparison of RFC 3339 committer timestamps, which carry a UTC offset, so
  `2026-01-01T09:00:00+02:00` and `2026-01-01T08:00:00+01:00` are the same instant and compare unequal — and a later
  instant can sort earlier.
  Any repository with mixed offsets was affected: a laptop that travelled, a CI box in UTC beside local commits,
  collaborators in different zones.
  The TUI already parsed these correctly, which is why only the JSON was wrong.
- **`--format=pretty` no longer hides the exclusion footers when no stack survives.**
  `render_pretty` returned early on an empty `stacks`, so in the one state where the footer *is* the answer — every
  bookmark's history contains a merge — it printed "No bookmark stacks found." and never said why.
- `--format`'s help omitted `committer_timestamp` from the sparse field list, in the first place an agent looks.
- `--keep`'s help and the selection module doc stated only the fold half of the selection rule; commits *above* the
  topmost mark are dropped from the submission, not folded into it.
- `docs/scripting.md`'s Python example printed its own lines after stakk's whenever stdout was a pipe.

### Other

- Documentation says how stack comments are actually identified, that a repo-supplied `stakk.toml` runs with your
  privileges, and what a submission includes, along with ordering and exit codes.
- Internal planning documents and other repo-only files are kept out of the published crate.
- Dead code deleted, and the rule for when a dead-code suppression is legitimate is now written down.
- *(deps)* update dependency uv to v0.12.4 ([#208](https://github.com/glennib/stakk/pull/208))

## [1.25.0](https://github.com/glennib/stakk/compare/v1.24.0...v1.25.0) - 2026-08-13

### Added

- *(remote)* support GitHub Enterprise Server hosts

### Fixed

- point bookmark_not_found at `stakk show`, document subcommand shadowing

### Other

- prune README redundancies and fix docs/ inaccuracies

## [1.24.0](https://github.com/glennib/stakk/compare/v1.23.0...v1.24.0) - 2026-08-13

### Added

- add `stakk docs` subcommand

### Other

- cache cargo artifacts and trim debug info

## [1.23.0](https://github.com/glennib/stakk/compare/v1.22.0...v1.23.0) - 2026-08-13

### Added

- *(comment)* draw the stack graph leaf-first in the default template
- *(cli)* non-interactive stack selection via --keep/--new flags
- *(submit)* make `--dry-run` fully inert by creating bookmarks in execute

### Fixed

- *(select)* check new bookmark names against every local bookmark

### Other

- *(deps)* update dependency rumdl to v0.2.55 ([#197](https://github.com/glennib/stakk/pull/197))
- update
- *(cli)* tighten `--help` text without losing facts
- gate Markdown formatting with rumdl
- document `ignore` placement, missing flags, and rumdl workflow
- use rumdl for formatting
- *(ci)* run ci regardless of base branch
- *(media)* update gif
- *(submit)* drop `analyze_submission`'s test-only folding arm

## [1.22.0](https://github.com/glennib/stakk/compare/v1.21.0...v1.22.0) - 2026-08-12

### Added

- *(show)* jj-log-style graph output and `--format=pretty|json`

### Other

- move graph layout out of select/ into graph/layout

## [1.21.0](https://github.com/glennib/stakk/compare/v1.20.0...v1.21.0) - 2026-08-12

### Added

- *(submit)* add `ignore` stack placement

### Fixed

- submit all bookmarked ancestors when a bookmark is passed explicitly

### Other

- *(deps)* update rust crate minijinja to v2.24.0 ([#187](https://github.com/glennib/stakk/pull/187))
- update gif

## [1.20.0](https://github.com/glennib/stakk/compare/v1.19.0...v1.20.0) - 2026-08-12

### Added

- *(select)* redesign branch-stack screen as jj-log-style single graph

### Fixed

- *(deps)* cargo update

### Other

- *(media)* remove the blank gap and clear-flash from the demo gif
- *(media)* automate README demo-gif recording

## [1.19.0](https://github.com/glennib/stakk/compare/v1.18.0...v1.19.0) - 2026-08-11

### Added

- *(submit)* separate execution output from the plan and persist stack-info status
- *(select)* collapse the TUI viewport on exit and size it per screen

## [1.18.0](https://github.com/glennib/stakk/compare/v1.17.2...v1.18.0) - 2026-08-08

### Added

- allow disabling comments
- *(select)* start bookmark cursor on the leaf row

### Fixed

- *(select)* clarify unchecked commit behavior in bookmark TUI

### Other

- *(deps)* upgrade octocrab to 0.54.1
- *(submit)* tighten the --stack-placement none cleanup path
- *(claude)* realign bookmark row state table
- *(deps)* update rust crate thiserror to v2.0.20 ([#170](https://github.com/glennib/stakk/pull/170))
- *(deps)* update rust crate clap to v4.6.6 ([#167](https://github.com/glennib/stakk/pull/167))
- *(deps)* update rust crate minijinja to v2.23.0 ([#169](https://github.com/glennib/stakk/pull/169))
- *(deps)* update rust crate clap_complete to v4.6.9 ([#168](https://github.com/glennib/stakk/pull/168))
- *(deps)* update dependency cargo:cargo-nextest to v0.9.143 ([#166](https://github.com/glennib/stakk/pull/166))
- *(deps)* update rust crate base64 to v0.23.1 ([#165](https://github.com/glennib/stakk/pull/165))
- *(deps)* update rust crate minijinja to v2.22.0 ([#164](https://github.com/glennib/stakk/pull/164))
- *(deps)* lock file maintenance ([#163](https://github.com/glennib/stakk/pull/163))
- *(deps)* update rust crate clap to v4.6.5 ([#161](https://github.com/glennib/stakk/pull/161))
- *(deps)* update dependency cargo:release-plz to v0.3.160 ([#160](https://github.com/glennib/stakk/pull/160))
- *(deps)* update dependency cargo:cargo-edit to v0.13.13 ([#158](https://github.com/glennib/stakk/pull/158))
- *(deps)* update dependency cargo:cargo-nextest to v0.9.140 ([#159](https://github.com/glennib/stakk/pull/159))

## [1.17.2](https://github.com/glennib/stakk/compare/v1.17.1...v1.17.2) - 2026-07-28

### Fixed

- *(deps)* update rust crate base64 to 0.23.0 ([#152](https://github.com/glennib/stakk/pull/152))

### Other

- *(deps)* update rust crate toml to v1.1.4 ([#155](https://github.com/glennib/stakk/pull/155))
- *(deps)* update rust crate clap_complete to v4.6.8 ([#154](https://github.com/glennib/stakk/pull/154))
- *(deps)* lock file maintenance ([#153](https://github.com/glennib/stakk/pull/153))
- *(deps)* update dependency cargo-binstall to v1.21.1 ([#151](https://github.com/glennib/stakk/pull/151))
- *(deps)* update rust crate tokio to v1.53.1 ([#150](https://github.com/glennib/stakk/pull/150))
- *(deps)* update rust crate serde_json to v1.0.151 ([#149](https://github.com/glennib/stakk/pull/149))
- *(deps)* update rust crate clap to v4.6.3 ([#148](https://github.com/glennib/stakk/pull/148))
- *(deps)* update rust crate thiserror to v2.0.19 ([#147](https://github.com/glennib/stakk/pull/147))
- *(deps)* update rust crate serde to v1.0.229 ([#146](https://github.com/glennib/stakk/pull/146))
- *(deps)* update rust crate futures to v0.3.33 ([#145](https://github.com/glennib/stakk/pull/145))
- *(deps)* update rust crate tokio to v1.53.0 ([#144](https://github.com/glennib/stakk/pull/144))
- *(deps)* update rust crate tokio to v1.52.4 ([#143](https://github.com/glennib/stakk/pull/143))
- *(deps)* update rust crate clap to v4.6.2 ([#142](https://github.com/glennib/stakk/pull/142))
- *(deps)* update dependency cargo-binstall to v1.21.0 ([#141](https://github.com/glennib/stakk/pull/141))

## [1.17.1](https://github.com/glennib/stakk/compare/v1.17.0...v1.17.1) - 2026-06-29

### Other

- *(deps)* lock file maintenance ([#130](https://github.com/glennib/stakk/pull/130))
- *(deps)* update actions/checkout action to v7
- *(deps)* update actions/cache action to v6

## [1.17.0](https://github.com/glennib/stakk/compare/v1.16.3...v1.17.0) - 2026-06-25

### Added

- *(tfidf)* order auto bookmark name terms by occurrence

## [1.16.3](https://github.com/glennib/stakk/compare/v1.16.2...v1.16.3) - 2026-06-22

### Fixed

- stop silently dropping selected bookmarks on jj-immutable revs

### Other

- *(deps)* lock file maintenance ([#126](https://github.com/glennib/stakk/pull/126))
- *(deps)* update dependency cargo-binstall to v1.20.1 ([#125](https://github.com/glennib/stakk/pull/125))
- *(deps)* update rust crate ratatui to v0.30.2 ([#124](https://github.com/glennib/stakk/pull/124))
- *(deps)* update rust crate minijinja to v2.21.0 ([#122](https://github.com/glennib/stakk/pull/122))
- *(deps)* lock file maintenance ([#121](https://github.com/glennib/stakk/pull/121))

## [1.16.2](https://github.com/glennib/stakk/compare/v1.16.1...v1.16.2) - 2026-06-10

### Fixed

- *(submit)* identify the segment in the missing-bookmark error
- *(errors)* add actionable help to auth and jj parse errors
- *(deps)* update rust crate octocrab to 0.53.0 ([#93](https://github.com/glennib/stakk/pull/93))

### Other

- remove remaining future-milestone dead code
- remove dead code and clarify expect() reasons
- *(deps)* update dependency cargo-binstall to v1.20.0 ([#115](https://github.com/glennib/stakk/pull/115))

## [1.16.1](https://github.com/glennib/stakk/compare/v1.16.0...v1.16.1) - 2026-06-08

### Fixed

- standardize capitalization of diagnostic help messages
- *(jj)* drop removed `--allow-new` flag from `jj git push`

## [1.16.0](https://github.com/glennib/stakk/compare/v1.15.0...v1.16.0) - 2026-06-08

### Added

- *(jj)* warn on startup when jj is older than the minimum supported version

## [1.15.0](https://github.com/glennib/stakk/compare/v1.14.1...v1.15.0) - 2026-06-06

### Added

- *(jj)* echo the failing command in jj error messages

## [1.14.1](https://github.com/glennib/stakk/compare/v1.14.0...v1.14.1) - 2026-05-25

### Other

- *(deps)* update rust crate http to v1.4.1 ([#104](https://github.com/glennib/stakk/pull/104))
- *(deps)* lock file maintenance ([#103](https://github.com/glennib/stakk/pull/103))
- *(deps)* update dependency cargo:cargo-dist to 0.32.0 ([#102](https://github.com/glennib/stakk/pull/102))
- *(deps)* update rust crate serde_json to v1.0.150 ([#101](https://github.com/glennib/stakk/pull/101))
- *(deps)* update rust crate minijinja to v2.20.0 ([#100](https://github.com/glennib/stakk/pull/100))
- *(deps)* lock file maintenance ([#99](https://github.com/glennib/stakk/pull/99))
- *(deps)* update rust crate clap_complete to v4.6.5 ([#98](https://github.com/glennib/stakk/pull/98))
- *(deps)* lock file maintenance ([#97](https://github.com/glennib/stakk/pull/97))
- *(deps)* update rust crate clap_complete to v4.6.4 ([#96](https://github.com/glennib/stakk/pull/96))
- *(deps)* update rust crate tokio to v1.52.3 ([#95](https://github.com/glennib/stakk/pull/95))
- *(deps)* update dependency cargo-binstall to v1.19.1 ([#94](https://github.com/glennib/stakk/pull/94))
- *(deps)* update rust crate tokio to v1.52.2 ([#92](https://github.com/glennib/stakk/pull/92))
- *(deps)* lock file maintenance ([#91](https://github.com/glennib/stakk/pull/91))
- *(deps)* update dependency cargo-binstall to v1.19.0 ([#90](https://github.com/glennib/stakk/pull/90))
- *(deps)* update dependency cargo-binstall to v1.18.1 ([#87](https://github.com/glennib/stakk/pull/87))

## [1.14.0](https://github.com/glennib/stakk/compare/v1.13.1...v1.14.0) - 2026-04-28

### Added

- *(submit)* `--trailers <keep|strip>` flag, `STAKK_TRAILERS` env var, and
  `trailers` config field for controlling whether git commit trailers
  (Signed-off-by, Co-authored-by, Refs, etc.) are kept in PR bodies or
  stripped before posting. Closes [#79](https://github.com/glennib/stakk/issues/79).

### Changed

- **Default trailer handling flipped from strip to keep.** v1.13.x stripped
  trailers from PR bodies unconditionally; v1.14 keeps them by default so
  squash-and-merge workflows that derive the merge message from the PR
  description preserve trailers in the merge commit. Set `--trailers strip`
  or `STAKK_TRAILERS=strip` (or `trailers = "strip"` in `stakk.toml`) to
  restore the previous behavior.

### Fixed

- *(submit)* multi-line trailer blocks are no longer mashed into a single
  line by markdown reflow when present in PR bodies.

## [1.13.1](https://github.com/glennib/stakk/compare/v1.13.0...v1.13.1) - 2026-04-28

### Other

- *(deps)* update rust crate tokio to v1.52.1 ([#80](https://github.com/glennib/stakk/pull/80))
- *(renovate)* enable automerge for low-risk updates
- Merge pull request #81 from glennib/renovate/octocrab-0.x-lockfile
- Merge pull request #82 from glennib/renovate/clap_complete-4.x-lockfile
- *(deps)* update rust crate clap_complete to v4.6.3
- fix clippy pedantic warnings

## [1.13.0](https://github.com/glennib/stakk/compare/v1.12.0...v1.13.0) - 2026-04-15

### Added

- strip git commit trailers from PR bodies

### Other

- Merge pull request #76 from glennib/renovate/tokio-1.x-lockfile

## [1.12.0](https://github.com/glennib/stakk/compare/v1.11.0...v1.12.0) - 2026-04-02

### Added

- add --sync-pr-content CLI flag, env var, and config field
- add update_pr_title to Forge trait and implementations

### Other

- add missing diagnostic help messages and sync display test

## [1.11.0](https://github.com/glennib/stakk/compare/v1.10.4...v1.11.0) - 2026-03-23

### Added

- *(cli)* rename --experimental-bookmark-command to --bookmark-command
- *(select)* add schema_version to bookmark command JSON protocol

### Fixed

- *(select)* add command-level timeout, timeout error, and multiline detection for bookmark commands

### Other

- polish help text and docs
- *(cli)* add example command to bookmark command help text
- *(select)* add toggle cycle and skip-logic tests for custom bookmark command

## [1.10.4](https://github.com/glennib/stakk/compare/v1.10.3...v1.10.4) - 2026-03-22

### Other

- upgrade windows-sys
- cargo update incompatible
- cargo update
- pin mise tools
- upgrade dist
- *(deps)* update actions/checkout action to v6
- ignore auto-generated release workflow in renovate config

## [1.10.3](https://github.com/glennib/stakk/compare/v1.10.2...v1.10.3) - 2026-03-22

### Other

- show repository URL in --help output

## [1.10.2](https://github.com/glennib/stakk/compare/v1.10.1...v1.10.2) - 2026-03-22

### Fixed

- interleave push and base-update to prevent GitHub auto-close ([#35](https://github.com/glennib/stakk/pull/35))

## [1.10.1](https://github.com/glennib/stakk/compare/v1.10.0...v1.10.1) - 2026-03-22

### Fixed

- *(select)* bust BookmarkNameCache on r/R for UseCustom rows

### Other

- update gif
- update gif

## [1.10.0](https://github.com/glennib/stakk/compare/v1.9.0...v1.10.0) - 2026-03-22

### Added

- *(config)* add TOML config file support with layered precedence
- *(submit)* add --pr-mode flag with regular/draft variants

### Other

- Merge pull request #52 from glennib/gb-config-clap-addition

## [1.9.0](https://github.com/glennib/stakk/compare/v1.8.0...v1.9.0) - 2026-03-22

### Added

- r/R cycles within bookmark states, Space skips between state types

### Other

- update claude.md

## [1.8.0](https://github.com/glennib/stakk/compare/v1.7.0...v1.8.0) - 2026-03-22

### Added

- make bookmark help line context-aware
- add UserInput bookmark name mode with vim-like modal editing
- add reverse cycling (b) and reverse regenerate (R) in bookmark widget

### Fixed

- preserve tfidf variation index when cycling through bookmark states
- order stacks by most-recent committer timestamp

### Other

- use debug build instead of release in CI task

## [1.7.0](https://github.com/glennib/stakk/compare/v1.6.1...v1.7.0) - 2026-03-22

### Added

- add native auto bookmark naming with TF-IDF scoring

## [1.6.1](https://github.com/glennib/stakk/compare/v1.6.0...v1.6.1) - 2026-03-20

### Fixed

- fold unselected segments' commits into next retained segment
- filter unselected bookmarks from submission analysis

### Other

- clippy fix

## [1.6.0](https://github.com/glennib/stakk/compare/v1.5.0...v1.6.0) - 2026-03-18

### Added

- *(submit)* skip stack info for single-bookmark submissions

## [1.5.0](https://github.com/glennib/stakk/compare/v1.4.1...v1.5.0) - 2026-03-18

### Added

- *(comment)* add warning preamble and STAKK_REPO_URL constant
- *(submit)* add --stack-placement body mode for stack info in PR body

### Fixed

- *(select)* surface bookmark command errors in TUI subtitle

## [1.4.1](https://github.com/glennib/stakk/compare/v1.4.0...v1.4.1) - 2026-03-16

### Fixed

- ignore BrokenPipe on stdin write in bookmark command
- resolve clippy warnings from CacheEntry and CustomNameState refactor
- *(select)* add [*]custom to bookmark help line legend when bookmark command is configured

### Other

- replace UseCustom(String) with CacheEntry enum and CustomNameState

## [1.4.0](https://github.com/glennib/stakk/compare/v1.3.0...v1.4.0) - 2026-03-16

### Added

- *(select)* experimental custom bookmark name generation via external command

## [1.3.0](https://github.com/glennib/stakk/compare/v1.2.0...v1.3.0) - 2026-03-16

### Added

- *(select)* support multiple bookmarks per change in TUI selection

## [1.2.0](https://github.com/glennib/stakk/compare/v1.1.0...v1.2.0) - 2026-03-13

### Added

- show short change ID prefix in TUI selection screens

### Other

- Merge pull request #41 from glennib/renovate/clap-4.x-lockfile
- *(deps)* update rust crate clap_complete to v4.6.0
- remove jack-test from claude.md

## [1.1.0](https://github.com/glennib/stakk/compare/v1.0.0...v1.1.0) - 2026-03-12

### Added

- make graph-discovery revsets configurable via CLI and env vars

### Fixed

- exclude immutable changes from graph discovery

## [1.0.0](https://github.com/glennib/stakk/compare/v0.2.9...v1.0.0) - 2026-03-09

### Highlights at 1.0

- Automatic stack detection from jj change graph
- Three-phase submission pipeline (analyze → plan → execute)
- Interactive ratatui TUI for branch selection and bookmark assignment
- Works without pre-existing bookmarks — creates them on-the-fly
- Stack-awareness comments on every PR with customizable minijinja templates
- Idempotent — re-running is always safe, existing PRs are updated
- Dry-run mode to preview without touching GitHub
- Draft PR support
- PR titles and bodies from jj change descriptions
- Environment variable configuration (`STAKK_REMOTE`, `STAKK_DRAFT`, `STAKK_TEMPLATE`)
- Shell completions (bash, zsh, fish, elvish, powershell)
- All VCS operations through jj — no direct git usage

### Other

- address clippy pedantic lints
- add license texts (MIT, Apache-2.0)
- add crates.io metadata (keywords, categories)
- remove stale development documents

## [0.2.9](https://github.com/glennib/stakk/compare/v0.2.8...v0.2.9) - 2026-03-09

### Added

- unwrap hard-wrapped markdown in PR bodies

### Fixed

- respect env vars when running without subcommand

### Other

- move gif
- update gif

## [0.2.8](https://github.com/glennib/stakk/compare/v0.2.7...v0.2.8) - 2026-03-09

### Added

- discover unbookmarked heads in change graph

## [0.2.7](https://github.com/glennib/stakk/compare/v0.2.6...v0.2.7) - 2026-03-07

### Fixed

- clippy

### Other

- use miette diagnostic features fully
- update gif

## [0.2.6](https://github.com/glennib/stakk/compare/v0.2.5...v0.2.6) - 2026-03-04

### Other

- add template docs in help text

## [0.2.5](https://github.com/glennib/stakk/compare/v0.2.4...v0.2.5) - 2026-03-04

### Added

- *(cli)* add STAKK_REMOTE and STAKK_DRAFT env var support
- *(comment)* minijinja-based stack comment templating

### Other

- document env vars and stack comment templating

## [0.2.4](https://github.com/glennib/stakk/compare/v0.2.3...v0.2.4) - 2026-03-02

### Fixed

- *(deps)* update rust crate rand to 0.10

### Other

- Merge pull request #23 from glennib/renovate/rand-0.x

## [0.2.3](https://github.com/glennib/stakk/compare/v0.2.2...v0.2.3) - 2026-03-02

### Added

- *(select)* replace inquire with ratatui TUI selector

## [0.2.2](https://github.com/glennib/stakk/compare/v0.2.1...v0.2.2) - 2026-02-25

### Fixed

- handle null elements in tracking_target arrays from jj

## [0.2.1](https://github.com/glennib/stakk/compare/v0.2.0...v0.2.1) - 2026-02-20

### Added

- add shell completions subcommand and version output
- enable vim mode (j/k navigation) and disable filtering in interactive selection
- add help message to stage 1 of interactive bookmark selection
- support canonical SSH URLs (ssh://git@github.com/...) in remote parsing

### Other

- move gif to right after Features section for better visibility
- improve README layout — move gif to Quick Start, replace textual comment example with screenshot
- add interactive gif and PR comment screenshot to README
- update README to reflect submit as default command

## [0.2.0](https://github.com/glennib/stakk/compare/v0.1.1...v0.2.0) - 2026-02-20

### Added

- [**breaking**] default command is now interactive submit instead of show
- default command is now interactive submit instead of show
- confirm before submitting when a single bookmark is auto-selected
- two-stage inquire-based interactive bookmark selection
- add `stakk show` subcommand

### Other

- research report and roadmap for interactive selector viewport
- interactive bookmark selection for `stakk submit`
- add features to readme
- *(deps)* update rust crate clap to v4.5.60
- Add renovate.json

## [0.1.1](https://github.com/glennib/stakk/compare/v0.1.0...v0.1.1) - 2026-02-19

### Other

- add installation methods to README

## [0.1.0](https://github.com/glennib/stakk/compare/v0.0.1...v0.1.0) - 2026-02-19

### Added

- replace anyhow with concrete error types and miette rendering
- implement milestone 6 polish and quality of life
- implement three-phase submission pipeline (Milestone 5)
- implement GitHub auth and Forge trait (Milestones 3 & 4)
- implement change graph construction (Milestone 2)
- implement jj interface layer with typed output parsing
- add project skeleton with CLI, error types, and CI

### Fixed

- *(ci)* add pat for workflow trigger capability
- only show first line of commit description in status output
- deduplicate unsynced bookmarks from jj output

### Other

- rename crate from jack to stakk
- add cargo-dist and release-plz distribution pipeline
- fmt
- add colorized stack output to milestone 7 roadmap
- add README with project overview, usage, and stacking guide
- update roadmap
- add workflow conventions to CLAUDE.md
- add version control conventions and integration test sidequest
- add CLAUDE.md
- add jj-stack analysis and development roadmap
