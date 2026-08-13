# Plan: CLI v2 — one parse path, fully explicit selection

Status: accepted and in progress.
C1–C6 and C8 are decided.
C7 is superseded (no version bump; its policy moved into C8's contract).
C9 was added after C3 landed; its field list is decided.
C10 was added after C6 landed, from a token-precedence bug found while writing `docs/auth.md`.
Implements the "breaking" half of issue #202 and collects every other worthwhile break into one major release (v2.0.0).

## Goals

- A bare `stakk` launches the TUI, exactly as today.
- A bare `stakk submit` also launches the TUI
  (already true today; becomes the *only* meaning of `submit` without flags).
- The bookmark shortcut is gone: no `stakk <bookmark>`, no `stakk submit <bookmark>`.
  Non-interactive submission is spelled with the explicit `--keep`/`--new*` marks only.
- Work *with* clap, not against it: no `args_conflicts_with_subcommands`, no argv rewriting,
  no error-message interception, one parse path, every arg has exactly one home.
- An ergonomic, easily extensible surface: adding a subcommand, an alias,
  or a selection flag later must never collide with user data or require another break.

## Why now

Issue #202 documents the damage the current design does:

1. **Subcommand names shadow bookmark names.** `Cli` flattens `SubmitArgs` at the top level with a positional
   `bookmark`, so `submit`, `auth`, `show`, `completions`, `docs`, and `help` are all names a bookmark cannot use via
   the bare form — and the set grows with every new subcommand.
2. **A wrong command can run silently.**
   `stakk --draft show` submits a bookmark named `show` and creates real PRs.
3. **Flags before the subcommand die with a baffling conflict error**
   (`the subcommand 'main' cannot be used with: --dry-run`),
   because `args_conflicts_with_subcommands` is load-bearing: without it,
   flags parsed into the flattened copy are silently dropped when a subcommand is present (the dual-home hazard).

All three are consequences of the flatten.
Removing it eliminates the whole class by construction, and retires `args_conflicts_with_subcommands` with it.

## The new surface

```text
stakk                       # TUI (submit with defaults)
stakk submit                # TUI (same thing, named)
stakk submit --keep A --new qzvs=B ...   # non-interactive selection
stakk submit --dry-run --keep A          # inert plan preview
stakk show [--format=json]  # discovery, offline
stakk docs [topic]
stakk completions <shell>
```

Rules, stated once:

- Bare `stakk` (only global flags: `--config`, `--github-host`) means `stakk submit`.
- Everything else names a subcommand.
  Flags belong after the subcommand.
- The global flags are the exception: clap's `global = true` makes them insertable anywhere — before the subcommand
  (`stakk --github-host X submit`) or after it (`stakk submit --github-host X`).
  Only options that genuinely apply to *every* subcommand get `global = true`; `--dry-run` stays a submit option,
  because a global would also make `stakk show --dry-run` parse and be silently meaningless —
  the accept-and-ignore class this redesign eliminates.
- `stakk submit` with no selection flags is the TUI; with selection flags it is fully explicit.
  On a non-terminal stdin with no flags it fails with `stakk::not_interactive`.

## Changes

### C1 — retire the top-level flatten (required)

`Cli` becomes:

```rust
pub struct Cli {
    pub config: Option<PathBuf>,        // global, unchanged
    pub github_host: Option<String>,    // global, unchanged
    #[command(subcommand)]
    pub command: Option<Commands>,
}
```

- Delete the `#[command(flatten)] submit_args` field and `args_conflicts_with_subcommands`.
- `None` arm in `main.rs`: run submit with default `SubmitArgs`,
  obtained by parsing the synthetic argv `["stakk", "submit"]` through a clone of the **config-applied** `Command`
  (the one returned by `apply_config_defaults`, cloned before the real `get_matches` consumes it),
  then extracting `SubmitArgs` from the `submit` subcommand matches.
  This is ordinary clap usage — clap defaults, env vars, and injected config defaults all apply,
  and there is no sniffing of the real argv.
- **Do not reintroduce the pre-flatten bug.**
  Before 90718ef5cf97 (*fix: respect env vars when running without subcommand*),
  the `None` arm built `SubmitArgs::default()` by hand, bypassing clap — env vars were silently ignored,
  and config-injected defaults would be today.
  The flatten was that commit's fix; the synthetic parse is v2's replacement for it.
  Two hard rules keep the trap shut: `SubmitArgs` gets no `Default` impl
  (nothing to reach for by accident),
  and the `None` arm's args must come from a clap parse of the config-applied `Command` — never constructed directly.
- The global flags are unaffected by the synthetic argv: `--config` is pre-parsed from the real
  `std::env::args()`, and `--github-host` is read from the real parse's `Cli` field,
  so `stakk --github-host X` (bare, TUI) keeps working.
- `apply_config_defaults` simplifies: global defaults on the root, submit+graph defaults on the `submit` subcommand,
  graph defaults on `show`.
  The top-level `apply_submit_and_graph_defaults` call disappears.

Consequences:

- `stakk --draft`, `stakk --dry-run show`,
  `stakk --dry-run submit main` all fail loudly with clap's standard "unexpected argument" error — #202's diagnostic 1
  (silent wrong command) and diagnostic 2 (baffling conflict message) are both gone.
  We accept clap's stock wording; no interception.
- The dual-home hazard is gone: every submit arg exists exactly once.
- Tests: delete/convert the top-level parse tests
  (`pr_mode_toplevel_*`, `github_host_from_config_without_subcommand`, `selection_flags_parse_at_top_level`,
  `selection_flags_conflict_at_top_level`).
  Add: bare `stakk` parses to `command: None`; and — the regression test for the 90718ef5cf97 trap —
  the synthetic-default parse picks up an injected config default
  (mirror `pr_mode_config_draft_no_flag` through the `None` arm).
  That one test pins the mechanism: env vars ride the same clap parse, and clap's env handling is not ours to test
  (process-env mutation in unit tests is off-limits per the existing convention anyway).

### C2 — remove the positional bookmark from `submit` (required)

- Delete `SubmitArgs::bookmark` and the `conflicts_with = "explicit_marks"` coupling.
- `main.rs`: the `if let Some(name) = &args.bookmark` arm goes; submit always takes the selection path
  (empty spec → TUI, otherwise `resolve_bookmarks_explicitly`).
  The reserved-names query now runs on every submit, which the selection paths already did.
- Delete `submit::analyze_submission` — the no-fold phase-1 constructor existed only for the positional (issue #184).
  `analysis_from_selection` becomes the single phase-1 constructor.
- Delete the `stakk::submit::bookmark_not_found` error (only the positional produced it).
- Update the `stakk::not_interactive` help text: it currently says
  "pass the bookmark name explicitly: stakk submit \<BOOKMARK\>";
  point it at the selection flags and `stakk docs scripting` instead.

Consequences:

- The subcommand namespace stops being a compatibility surface.
  #202's standing constraint dissolves: future subcommands and clap aliases are free.
- `stakk -- <bookmark>` dies with the positional (there is nothing for `--` to disambiguate).
- Migration: `stakk submit BM` (every bookmarked boundary trunk→BM as its own PR) becomes
  `stakk submit --keep b1 --keep b2 ... --keep BM` — same PR set,
  with colinearity validated instead of the positional's silent first-stack pick.

### C3 — remove `--keep-all` (required)

With the positional gone, `--keep-all` is the last implicit construct in the selection language,
and it carries the most spec debt: the bare-form ambiguity rule
("one stack, or several that agree on their bookmarks"),
the `stakk::selection::keep_all_ambiguous` and `stakk::selection::no_marks` diagnostics,
and the explicit-beats-bulk skip rule.
Removing it makes the selection language uniform: **every PR boundary is named on the command line.**

- Delete the `keep_all` field, `SelectionSpec::keep_all`, `keep_all_expansion`, `describe_stack`
  (used only by the ambiguity error), the two error variants, and their tests.
- Removal is the safe direction for a major release:
  re-adding a bulk flag later (this shape or a better one, e.g. `--tip REV`) is non-breaking,
  while keeping it and removing it later needs another major.

Cost, stated honestly: "submit my whole stack non-interactively" becomes verbose.
The scripted idiom is one discovery call away:

```sh
stakk submit $(stakk show --format=json \
  | jq -r '.stacks[0].segments[]
           | select(.bookmark_names | length > 0)
           | "--keep=\(.bookmark_names[0])"')
```

One `--keep` per *segment*, not per bookmark name: a commit can carry several bookmarks,
and two `--keep`s on one commit are two marks on one boundary (`stakk::selection::duplicate_mark`).
Unbookmarked segments are skipped — they fold into the boundary above, which is what they did under `--keep-all` too.

Agents already run `stakk show --format=json` first per `docs/scripting.md`, so for them the cost is near zero.
If the verbosity turns out to hurt humans, a replacement can ship in any 2.x.

### C4 — remove `--draft` and `STAKK_DRAFT` (required)

`--draft` duplicates `--pr-mode=draft` with a special override rule: draft-ness wins from whichever source sets it,
even over an explicit `--pr-mode regular`.
`docs/config.md` has to tell users to *unset* `STAKK_DRAFT` rather than override it — a documented footgun.

- Delete the `draft` field, `STAKK_DRAFT`, and the `SubmitArgs::pr_mode()` helper (callers read the field).
- One knob remains — `--pr-mode` / `STAKK_PR_MODE` / `pr_mode` — with the standard CLI > env > config precedence.
- Delete the `--draft` interplay paragraph in `docs/config.md`.

### C5 — rename `template` → `template_path` (required)

`--template` takes a *path*, but the name reads as inline template content.
Rename all four touchpoints: `--template-path`, `STAKK_TEMPLATE_PATH`, `template_path` in TOML, docs.
This frees `template` for a possible future inline-template option and makes the existing one self-describing.

### C6 — remove `stakk auth` entirely, replace with `stakk docs auth` (required)

`auth setup` prints a static wall of `println!`s; `auth test` is a connectivity probe
(resolve remote → resolve token → call the API and print the username).
Both go; a `DocTopic::Auth` backed by `docs/auth.md` takes over the prose, gaining rumdl formatting, runtime reflow,
and the `stakk docs` index for free.

- Delete `src/cli/auth.rs`, the `Commands::Auth` variant, and `auth_test`/`auth_setup` in `main.rs`
  (the `runs_jj` match loses its `Auth` arm).
  The `src/auth.rs` token-resolution module stays — submit uses it.
- What `auth test` did is still reachable without it:
  `gh auth status --hostname <host>` checks the token the same way stakk resolves it,
  since stakk delegates to `gh auth token` first.
  A `stakk submit --dry-run` **with selection flags** exercises the full remote→token→API chain read-only,
  because the forge is queried in the plan phase.
  Corrected from this plan's first draft, which said "any `stakk submit --dry-run`": a bare `--dry-run` reaches the TUI
  (or `stakk::not_interactive`) before the plan phase, so it resolves the remote and token but never calls the API.
  `docs/auth.md` says this, and takes over `docs/config.md`'s "the quickest way to confirm the setup" guidance from
  `stakk auth test`.
- New doc topic follows the established recipe: `DocTopic` variant, `docs::source` arm, the file.

### C7 — state that the jj floor is not semver-guarded (required)

Superseded: this was originally "bump `MIN_SUPPORTED_JJ_VERSION` to the latest jj at ship time".

v2 changes no `jj` invocation — the CLI surface moves, the VCS interface does not —
so there is nothing this release needs from a newer jj,
and raising the floor to 0.44.0 would be a bump with no engineering behind it.
A floor raised for its own sake costs users on older jj and buys nothing.

What is worth settling is the *policy*, which belongs in C8's contract:
`MIN_SUPPORTED_JJ_VERSION` is warn-only and raising it is **not** a breaking change.
Saying so once means future bumps can land in any minor release,
on the release where a new jj feature is actually adopted, rather than being saved up for a major.

`MIN_SUPPORTED_JJ_VERSION` therefore stays 0.39.0 in v2.0.0.

### C8 — state the stability contract, and exempt `stakk docs` from it (required)

v2 should say out loud what a "breaking change" even means for stakk, so v3 discussions have a baseline.
A short **Stability** section in the README
(the project's contract document — deliberately *not* a doc topic,
since doc-topic text is exactly what the contract declares unstable) declaring:

- **Stable** (semver-guarded): subcommand names and flags, environment variables, config keys and their defaults,
  the `stakk show --format=json` document under its `schema_version`, the `--bookmark-command` JSON input under its
  `schema_version`, diagnostic codes (`stakk::...`), and exit codes.
- **Not stable**: the *rendered text* of `stakk docs` and `--help`, the pretty `stakk show` output,
  the TUI layout and keybindings, spinner/progress text, and error message *wording*
  (the codes are the contract, not the prose).
  Doc topics may be added, reworded, or restructured in any release;
  only the `stakk docs <topic>` invocation shape itself is stable.
- **Not a breaking change**: raising `MIN_SUPPORTED_JJ_VERSION` (C7).
  The check is warn-only — stakk runs against an older jj and says so — so a bump can land in any release,
  on the release that actually adopts a newer jj's behaviour.

This is what makes C6 safe going forward: setup guidance lives in a place we can freely rewrite.

### C9 — split `stakk show` JSON into sparse and full (required)

Reverses the "considered and rejected" entry below.
The JSON document is the interface agents actually consume,
and today it carries far more than pinpointing a segment needs: full commit-message bodies, author blocks,
and `files[]` dominate it.
Measured on this repo over six commit entries: **13,704 bytes full vs 1,659 sparse — roughly 8×**,
and the ratio grows with commit-message length.
For an agent paying by the token on every discovery call, that is the difference that matters.

Shape:

- **`--format=json` becomes sparse** — enough to pinpoint a segment and feed `stakk submit`, nothing more.
- **`--format=json-full` is the current document**, plus the new `title` field.
- **Sparse is a strict subset of full.**
  Every field in sparse appears in full, with the same name, type, and meaning.
  One schema, two projections — never two dialects.

Field changes:

- Add `title` to every commit: the first line of the commit message.
- `description` keeps its current meaning — the *full* message, title line included.
  Adding `title` alongside is purely additive, so `description` consumers are unaffected by the field itself;
  they break only by being dropped from sparse.
- Sparse commit: `change_id`, `short_change_id`, `title`, `is_immutable`, `local_bookmark_names[]`,
  `is_boundary`, `is_leaf`.
- Sparse drops per commit: `commit_id`, `description`, `author`, `files[]`.
- Sparse keeps top-level `schema_version`, `default_branch`, `remotes[]`, `excluded_bookmark_count`
  (all small, and `remotes[]` says where PRs will go).
- `schema_version` → `2`, for both projections.

Decided (both were open when C9 was drafted):

- **`change_id` is in sparse.**
  The full id is unambiguous where a prefix is not,
  and ~32 bytes per commit is noise against a field set that already saves an order of magnitude.
- **`files[]` is not in sparse.**
  It is one of the two largest fields; an agent that needs to see what a commit touches asks for `--format=json-full`.

Consequences:

- `--format=json` changing content is the break.
  Scripts reading `.description`, `.author`, `.files` or `.commit_id` from `--format=json` move to `--format=json-full`.
- `docs/scripting.md`'s **The `stakk show --format=json` document** section documents both projections,
  and its jq examples must be re-checked against sparse (the `local_bookmark_names` example still works;
  it uses only sparse fields).
- `src/show/snapshots/stakk__show__tests__json_snapshot.snap` will change and a full-format snapshot is likely added.
  **Snapshots are for the repo owner to review and accept — the implementor must not accept them.**
- `ShowFormat` gains a variant; `--format` is a `value_enum`, so `--help` and completions follow.
  `stakk show` has no env var or config key for `format`, so §8's four-touchpoint rule does not apply.

### C10 — align the github.com token fallback order with the GitHub CLI (required)

Found while writing `docs/auth.md` for C6.
`gh help environment` states the precedence as `GH_TOKEN`, `GITHUB_TOKEN` for github.com and `GH_ENTERPRISE_TOKEN`,
`GITHUB_ENTERPRISE_TOKEN` for an Enterprise host.
`env_sources` in `src/auth.rs` follows that for Enterprise but **inverts it for github.com**,
trying `GITHUB_TOKEN` before `GH_TOKEN`.
The code comment states the reason plainly —
"github.com keeps stakk's long-standing `GITHUB_TOKEN` before `GH_TOKEN` order" — which is legacy, not a rationale.

So github.com is inconsistent both with gh and with stakk's own Enterprise branch.
Empirically, with both variables exported,
`gh auth token` returns `GH_TOKEN` while stakk's fallback would return `GITHUB_TOKEN`:
the same environment resolves to two different tokens depending on whether gh happens to be installed.

- Swap the github.com pair in `env_sources` to `GH_TOKEN`, then `GITHUB_TOKEN`.
- Replace the legacy comment with the real rule: both pairs follow the GitHub CLI's documented order,
  so stakk's fallback and gh's own answer agree.
- Update the precedence assertions in `src/auth.rs`'s tests, and the token tables in `docs/auth.md`
  and `docs/config.md`.
  `docs/auth.md` can drop its "the two do not agree, so export one" caveat, which exists only to
  describe this bug.

Narrow but real break: it changes which token is used when *both* variables are set *and* gh is absent
or unauthenticated for the host.
With gh present, gh already decides and nothing changes.

Not addressed here: gh also applies `GH_TOKEN`/`GITHUB_TOKEN` to `ghe.com` subdomains,
which stakk routes to the Enterprise pair.
That is a separate divergence, out of scope for a precedence fix.

## Considered and rejected

- **Argv injection** (treat any bare `stakk <flags>` as `stakk submit <flags>`): fights clap,
  reintroduces a second parse concept, and turns typos into submit args.
  Bare `stakk` stays a special case only when it is *entirely* bare.
- **Intercepting clap errors** to rewrite the flags-before-subcommand message: unnecessary once the conflict setting is
  gone; clap's stock error is loud and accurate.
- **`--tip REV` shorthand** (the no-fold positional semantics as an explicit flag): plausible future addition,
  but speculative surface does not belong in the release that exists to shrink surface.
  Non-breaking to add later.
- **Changing default revsets, `--sync-pr-content`, or `--stack-placement` defaults**: current defaults are deliberate
  (see CLAUDE.md key decisions); no evidence they are wrong.
- **Renaming `--new-auto`/`--new-command`**: already consistent with `--new` and `--bookmark-command`.
- ~~**`stakk show` JSON schema changes**: nothing rides along; `schema_version` stays 1.~~ **Reversed — see C9.**
  The contract in C8 declares the JSON document stable *under* its `schema_version`,
  which makes a later change cost a `schema_version` bump and a second migration event for scripts.
  Doing it inside v2.0.0 costs one changelog line instead.

## Migration table (changelog material)

| v1                                            | v2                                                          |
| --------------------------------------------- | ----------------------------------------------------------- |
| `stakk`                                       | unchanged — TUI                                              |
| `stakk submit`                                | unchanged — TUI                                              |
| `stakk <bookmark>` / `stakk -- <bookmark>`    | `stakk submit --keep <b1> ... --keep <bookmark>`             |
| `stakk submit <bookmark>`                     | same as above                                                |
| `stakk submit --keep-all`                     | enumerate `--keep`s (jq idiom above)                         |
| `stakk --dry-run <...>` (flags before subcmd) | `stakk submit --dry-run <...>`                               |
| `--draft` / `STAKK_DRAFT`                     | `--pr-mode draft` / `STAKK_PR_MODE=draft` / `pr_mode` in TOML |
| `--template` / `STAKK_TEMPLATE` / `template`  | `--template-path` / `STAKK_TEMPLATE_PATH` / `template_path`  |
| `stakk show --format=json` (full document)    | `stakk show --format=json-full` (sparse is the new `json`)   |
| `GITHUB_TOKEN` beats `GH_TOKEN` (no gh)       | `GH_TOKEN` beats `GITHUB_TOKEN`, matching the GitHub CLI      |
| `stakk auth setup`                            | `stakk docs auth`                                            |
| `stakk auth test`                             | `gh auth status --hostname <host>`, or `stakk submit --dry-run` *with selection flags* |

Removed machine-readable surface (scripts matching diagnostic codes): `stakk::submit::bookmark_not_found`,
`stakk::selection::keep_all_ambiguous`, `stakk::selection::no_marks`.

## Implementation order

One `feat!:` conventional commit per change, each with a `BREAKING CHANGE:` footer,
so release-plz produces v2.0.0 with a changelog entry per break.
Order (each lands green through `mise run ci`):

1. **C2** — remove the positional (`submit.rs`, `main.rs`, `submit/mod.rs`, `error.rs`, tests).
2. **C1** — retire the flatten (`cli/mod.rs`, `main.rs` `None` arm, `apply_config_defaults`, tests,
   including the 90718ef5cf97 regression test).
3. **C3** — remove `--keep-all` (`submit.rs`, `select/explicit.rs`, tests).
4. **C4** — remove `--draft`/`STAKK_DRAFT`.
5. **C5** — rename `template` → `template_path` (all four touchpoints in one commit, per CLAUDE.md §8).
6. **C6** — remove `stakk auth`, add the `auth` doc topic (`cli/auth.rs` deleted, `DocTopic::Auth`,
   `docs/auth.md`, `main.rs` dispatch and `runs_jj`).
7. **C7** — no code change; the jj-floor policy ships as part of C8's contract.
8. **C9** — split `stakk show` JSON into sparse/full (`src/show/`, `ShowFormat`, `docs/scripting.md`).
   After C6 so the doc-topic machinery is settled, before C8 so the contract describes the final schema.
   Produces snapshot changes for the repo owner to accept.
9. **C8** — the stability contract text (a `docs:` commit; ship it in the same release so v2.0.0 opens with the
   contract in place), including the C7 policy statement and C9's `schema_version: 2`.
10. **Docs sweep** (can be folded into the commits above where the four-touchpoint rule demands it;
   the README/scripting rewrite may be its own `docs:` commit).

### Docs sweep checklist

- **README**: rewrite `### stakk submit [bookmark]` and every `stakk submit my-feature` example around the TUI and
  selection flags; delete **Bookmark names that collide with subcommands**; update **Non-interactive selection**;
  re-check the four-key `stakk.toml` sample (C5) and feature bullets.
- **docs/scripting.md**: delete **Always name the subcommand**; drop `--keep-all` rows and the
  `keep_all_ambiguous`/`no_marks` error rows; update examples.
- **docs/config.md**: env table (`STAKK_DRAFT`, `STAKK_TEMPLATE`), the `--draft` interplay paragraph,
  the selection-flags note, the annotated `stakk.toml` block (C5); the GitHub Enterprise Server section leans on
  `stakk auth test` for verification and the env table points at `stakk auth setup` — both repoint at
  `stakk docs auth` and its alternatives (C6).
- **docs/template.md**: `--template` mentions (C5).
- **CLAUDE.md**: §8's bare-form note, the "Two phase-1 constructors" gotcha (now one),
  the Key Decisions entries touching the positional and `--keep-all`, the subcommand-shadowing sentence.
- **scripts/record-demo.py**: types a bare `stakk`, which stays valid; screens, keys,
  and final output strings are untouched by this plan, so no re-record is expected — verify, don't assume.
- **Completions** regenerate from clap automatically.
- **Issue #202**: closed by the C1/C2 commits (`Closes #202` in the PR body).

## Decision record

All eight changes (C1–C8) ship in v2.0.0; none are optional.

- **No deprecation pre-release on 1.x.**
  Given stakk's audience size, the changelog, migration table, and rewritten docs suffice;
  v2.0.0 is the first and only signal.
- **`--keep-all` goes without a replacement.**
  The jq idiom covers scripts; if the verbosity hurts humans in practice, a bulk flag
  (this shape or `--tip REV`) can ship in any 2.x — re-adding is non-breaking, so removal carries no risk.
- **`stakk auth` goes entirely**, accepting the loss of the `auth test` probe:
  `gh auth status --hostname <host>` and `stakk submit --dry-run` cover it,
  and `docs/auth.md` documents both.
- **The stability contract lives in the README**, not in a doc topic.
