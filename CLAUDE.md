# CLAUDE.md — stakk

## Project Overview

**stakk** is a Rust CLI tool that bridges Jujutsu (`jj`) bookmarks to GitHub stacked pull requests.
It complements jj by turning local bookmark state into coherent GitHub PRs with correct stacking order.

## Current Status

All core features are complete: stack detection, three-phase submission, interactive TUI selection,
fully explicit non-interactive selection via `--keep`/`--new*`, offline `stakk show`
(pretty graph + versioned JSON in sparse and full projections),
four stack-placement modes, stack comment templating, layered config
(CLI > env > repo/user TOML), bundled documentation via `stakk docs`, and comprehensive error handling.

The CLI surface is the one `plans/cli-v2.md` describes: `stakk` and `stakk submit` both open the TUI,
every PR boundary of a non-interactive submission is named with a selection flag, and there is no positional bookmark,
no `--keep-all`, no `--draft`, and no `stakk auth`.
Which parts of that surface are semver-guarded is stated in the README's **Stability** section —
the contract lives there, not in a doc topic, because doc-topic text is exactly what it declares unstable.

Versions are managed by release-plz from conventional-commit messages.
Never edit `CHANGELOG.md` or the `version` field by hand, and do not pin a version number here — release-plz owns it.

## Testing

- **Unit/integration tests**: `cargo nextest run --all-targets`.
- **Final pre-commit check**: `mise run ci` — run this after implementing plans
  and before committing.

## Development Principles

### 1. Never call git directly

All git operations go through `jj` commands (`jj git push`, `jj git remote list`, `jj git fetch`, etc.).
No `git` CLI calls, no `git2`, no `gix`.

### 2. Shell out to jj, don't link jj-lib

The `jj` CLI is the stable interface.
Shell out and parse JSON/structured output with serde.
Always pass `--config 'ui.paginate=never'`.

### 3. Forge trait

All forge interaction goes through a `Forge` trait.
The core submission logic must never import GitHub-specific types directly.

### 4. Idempotent operations

Re-running any command must be safe.
`submit` updates existing PRs rather than creating duplicates.
Stack comments are identified by embedded metadata.

### 5. Boring solutions over clever abstractions

Prefer simple, obvious code.
Three similar lines are better than a premature abstraction.

### 6. Test with fixtures

Capture real `jj` and GitHub API output as test fixtures.
Tests should run without a live jj repo or GitHub access.

### 7. No jj-stack compatibility

stakk uses its own comment metadata format
(`STAKK_STACK` prefix), its own serde field naming (snake_case), and its own comment footer.

### 8. CLI args, env vars, and config travel together

Every user-facing `submit` arg has four touchpoints that must stay in sync.
When adding, renaming, or changing the default of one, update all of them in the same change:

1. **`src/cli/submit.rs`** — the clap field with `#[arg(long, env = "STAKK_…",
   default_value = …)]` and a `verbatim_doc_comment` doc.
2. **`src/config/mod.rs`** — add `Option<T>` field on `Config`, default to
   `None`, and merge in `Config::merge`.
3. **`src/cli/mod.rs`** — wire it into `apply_submit_defaults`
   (or `apply_graph_defaults`)
   via `set_default(...)`, and add tests mirroring the `sync_pr_content_*` set: `default`, `config_*`,
   `cli_overrides_config`.
   Extend `toml_deserialize_full`.
   A `global = true` arg on `Cli` (like `--config` and `--github-host`) is defined on the *root* command only,
   so its default goes through `apply_global_defaults`, not `apply_submit_defaults` —
   `mut_arg` on a subcommand would panic with "Argument is undefined".
4. **`docs/config.md`** — two places:
   - the annotated `stakk.toml` example block (the exhaustive one; document the default in the comment),
   - the **Environment variables** table.

   `README.md` deliberately carries *no* flag or env-var reference tables — `stakk --help`,
   `stakk <subcommand> --help` and `docs/config.md` are the reference.
   Only touch the README when the option changes something it describes in prose: the four-key `stakk.toml` sample,
   the **Stack info placement** summary, **GitHub Enterprise Server**, **Non-interactive selection**,
   **PR titles and bodies**, **Custom bookmark names**, or **Immutable commits**.
   Deeper reference material for those lives in `docs/template.md` and `docs/scripting.md`,
   which each README section links to alongside its `stakk docs <topic>` command.

A change that lands in only some of these will silently drop config-file support, fail to appear in `--help` defaults,
or stay invisible in the docs.

The README describes only encouraged flows.
`submit` takes no positional bookmark: `stakk` and `stakk submit` both mean the TUI,
and non-interactive submission is spelled with the `--keep`/`--new*` selection flags.

## Architecture

```text
src/
├── main.rs          # CLI entry point (clap)
├── auth.rs          # Per-host GitHub token resolution (gh CLI, env vars)
├── cli/             # clap subcommand definitions (Cli, SubmitArgs, ShowArgs, GraphArgs, DocTopic)
├── config/          # TOML config discovery, merging, and clap-default injection
├── docs/            # `stakk docs` — include_str!s docs/*.md, renders them, generates the topic index
├── markdown/        # Markdown transforms shared by submit/ and docs/
│   ├── unwrap/      # unwrap_markdown: hard-wrapped prose → soft-wrapped paragraphs
│   └── wrap.rs      # wrap_markdown: prose → folded to a target width
├── jj/              # jj CLI interface — all VCS ops go here
│   ├── mod.rs       # Jj<R: JjRunner> — every jj invocation
│   ├── runner.rs    # JjRunner trait + real/mock runners
│   ├── types.rs     # serde structs for jj template output
│   ├── remote.rs    # Remote URL parsing + GitHub host gate (GitHubRepo, api_base_uri)
│   └── version.rs   # jj version parsing + minimum supported version
├── forge/           # Forge trait + GitHub implementation (octocrab)
│   ├── mod.rs       # Forge trait, forge-agnostic types, ForgeError
│   ├── github.rs    # GitHubForge implementation
│   ├── comment.rs   # Stack comment formatting, parsing, and template context
│   └── default_comment.md.jinja  # Default minijinja template for stack comments
├── graph/           # Change graph construction (ChangeGraph, BookmarkSegment, BranchStack)
│   ├── types.rs     # Graph data types shared by select/, show/, and submit/
│   └── layout.rs    # Convert ChangeGraph → jj-log-style row list (commit tree + display rows)
├── select/          # Interactive TUI selection (ratatui inline viewport)
│   ├── mod.rs       # Public API: resolve_bookmark_interactively(), SelectionResult
│   ├── app.rs       # App state machine, event loop, terminal init
│   ├── graph_widget.rs  # Screen 1: jj-log-style graph widget (leaf selection, collapsing, scrolling)
│   ├── bookmark_widget.rs # Screen 2: bookmark toggle/assignment widget
│   ├── explicit.rs  # Non-interactive selection via --keep/--new/--new-auto/--new-command
│   ├── bookmark_gen.rs # Bookmark validation and external command execution
│   ├── tfidf.rs     # TF-IDF algorithm for auto-generated bookmark names
│   └── event.rs     # crossterm key event mapping to app actions
├── show/            # `stakk show` rendering: pretty graph + schema-versioned JSON in two
│                    # projections, sparse (`--format=json`) and full (`--format=json-full`)
│                    # (offline, pure jj)
├── submit/          # Three-phase submission (analyze → plan → execute)
└── error.rs         # Error types (thiserror)
```

There is intentionally no `git/` module.

## Conventions

### Rust

- Edition 2024.
- Use `cargo nextest run` for testing, not `cargo test`.
- Prefer `cargo run --bin stakk` and `cargo build --bin stakk` over `-p stakk`.
- Find built binaries with:
  `cargo build --release --message-format json | jq -r 'select(.executable | . == null | not) | .executable'`
- **Never use `#[allow(...)]`**.
  Use `#[expect(..., reason = "...")]` instead,
  which requires a reason and warns when the expectation becomes unnecessary.

### Formatting

- `rustfmt.toml` uses nightly-only options (`format_strings`, `group_imports`,
  `imports_granularity`, `wrap_comments`, `doc_comment_code_block_width`).
- Run `mise run fmt:nightly` (or `cargo +nightly fmt --all`) for full
  formatting locally.
- **Always run `cargo +nightly fmt --all` before committing.**

### Markdown

- **Always run `mise run md` (`rumdl fmt .`) after modifying any Markdown file**,
  then re-read what it changed — it rewrites line breaks.
- [`.rumdl.toml`](.rumdl.toml) is the authority on settings and scope: 120-column lines,
  semantic line breaks (one clause per line), and an `exclude` list.
- Nothing under `src/` is formatted.
  Markdown there is payload, not prose — `forge/default_comment.md.jinja` and Markdown test fixtures —
  and reflowing it would change what stakk writes to GitHub or what the tests assert.
  `CHANGELOG.md` is excluded too; release-plz owns it.
- `mise run md` formats, `mise run md:check` reports, and `md:check` is part of `mise run ci` —
  unformatted Markdown fails the build.
- `rumdl` is version-pinned in `mise.toml` because it gates CI; bump it deliberately,
  and run `mise run md` in the same change so the reformat does not land as unrelated churn.
- Semantic line breaks mean prose edits should stay on their own line rather than re-wrapping a paragraph;
  it keeps docs diffs readable.
- `docs/*.md` *is* rumdl-formatted, unlike `src/`, even though it is `include_str!`ed into the binary.
  Semantic line breaks are harmless there because `stakk docs` re-flows prose at runtime for a terminal
  (see the `stakk docs` entry under Patterns & Gotchas).

### Version Control

- This repo uses `jj` (Jujutsu) for version control.
  Prefer `jj` over `git`.
- Before starting a new logical piece of work, verify a clean slate with `jj status`.
  If the current change is not empty, prompt the user or run `jj new`.
- Use `jj commit -m "message"` to finalize a change.
- Use `jj tug` to move the main bookmark forward to `@-` after committing.
- Push with `jj git push --bookmark main`.

### Error Handling

- Use `thiserror` + `miette::Diagnostic` for defining error enums everywhere.
- Concrete error types all the way up; `miette::Report` only at the `main()`
  boundary for rendering.
- No `anyhow` — every error is a concrete type with `Diagnostic` metadata.
- Use `#[diagnostic(help(...))]` for actionable advice on all variants.
  Use `#[diagnostic(code(stakk::...))]` for machine-readable error identifiers.

### jj Interface

- Always run jj with `--config 'ui.paginate=never'`.
- Use `--template` for structured/JSON output where available.
- Define serde structs for every piece of jj output we consume.
- Paginate large output (100 items at a time) to avoid memory issues.

## Selection TUI (select/)

Two-screen inline viewport TUI for choosing a branch path and assigning bookmarks to commits.

### Screens

1. **GraphView** — Select a leaf node (branch tip) from a jj-log-style graph: one row per commit,
   `│ `-cell edge gutter on the left, `├─╯` connector rows where a sibling subtree merges into its parent's column.
   Every row permanently carries its bookmark names and `"description"`;
   navigation only re-colors the selected trunk→leaf path.
   Glyphs: `●` selected path (cyan; the leaf is bold with a trailing `◀`), `○` other commits
   (dark gray), `◆` trunk, `⋯ n commits` collapsed runs.
   Sibling subtrees are ordered newest-first by committer timestamp
   (jiff-parsed, offset-aware; tiebreak on the subtree root's change_id), so leaf 1 is the most recent stack.
   Runs of consecutive unselected commits that carry no bookmark and are neither leaves
   nor trunk collapse into `⋯ n commits`; the selected path is always fully expanded.
   When the (collapsed) graph exceeds the viewport, it scrolls to anchor the selected leaf at the top.
   The title row shows a right-aligned `leaf i/n` indicator;
   the viewport is sized to the max collapsed height over all leaves so leaf switches never resize it.
   Navigate leaves with `←`/`→` (`h`/`l`) or `↑`/`↓` (`k`/`j`), wrapping; confirm with Enter.
   Help line: `←→↑↓/hjkl leaf  Enter select  q/Esc quit`

2. **BookmarkAssignment** — Assign bookmark names to each commit in the selected path.
   Navigate rows with `↑`/`↓` (`j`/`k`).
   Rows are displayed in reverse order (leaf at top, trunk at bottom), and the cursor starts on the leaf row.
   Confirm with Enter, cancel back to GraphView with Esc/`q`.

### Bookmark Row State Cycle

Each non-trunk row cycles through state *types* via Space (forward) / `b` (reverse).
Each type is a single stop — Space never cycles within a type:

```text
[x]use → [~]auto → [>]type → [+]new → [*]custom → [ ]skip
```

(`[*]custom` only present when `--bookmark-command` is configured.)

| Checkbox | State            | Color        | Description                                                 |
|----------|------------------|--------------|-------------------------------------------------------------|
| `[x]`    | UseExisting(idx) | green, bold  | Use an existing bookmark; `r`/`R` cycles when >1            |
| `[~]`    | UseTfidf         | blue, bold   | TF-IDF name from commit description + files                 |
| `[>]`    | UserInput        | lt-yellow    | Manual entry — press `i` to edit, validates live            |
| `[+]`    | UseGenerated     | yellow, bold | Auto `stakk-<change_id[:12]>`                               |
| `[*]`    | UseCustom        | cyan, bold   | External `--bookmark-command` with async spinner            |
| `[ ]`    | Unchecked        | dark gray    | No PR of its own; the commit is included in the PR above it |

States are **skipped** when they would produce no name or a name that is already taken — by another state on the row,
or by *any* local bookmark in the repo (the reserved set; see Patterns & Gotchas).
UseTfidf, UseGenerated and UseCustom all follow this rule.
Confirming re-checks it: a new name in the reserved set fails with `Bookmark already exists: <name>`,
and a typed name renders red while it collides.

### Locked Rows (immutable commits)

A row is **locked** when `is_immutable && existing_bookmarks.is_empty()` (`BookmarkRow::is_locked()`):
its commit is jj-immutable and carries no bookmark known to the graph,
so a bookmark created there would be filtered out by the default bookmarks revset
(`~ immutable()`) on every subsequent run — stakk would create a PR it can never see or manage again.
Locked rows are permanently `[ ]` Unchecked
(dark gray)
and render a hint — `(immutable — bookmark <names> excluded by --bookmarks-revset)`
when the commit carries filtered-out bookmarks (`excluded_bookmarks`), else `(immutable)`.
All mutating keys (Space, `b`, `r`, `R`, `i`) no-op; the row stays focusable so the hint is discoverable,
and the help line shows `immutable — locked` instead of the cycle keys.
Immutable rows that *are* segment boundaries (possible under custom revsets without `~ immutable()`) are not locked.
Accepted limitation: locking keys on commit immutability, not revset semantics —
under a custom revset that includes immutable commits, a bare immutable row stays locked;
create the bookmark outside the TUI and the row unlocks as a boundary.

### Variation (`r`/`R`)

`r`/`R` cycles *within* the current state type:

- **UseExisting**: cycles through existing bookmarks (only when >1 exist).
- **UseTfidf**: cycles through up to 6 name variations.
- **UseCustom**: clears the cache and re-fires the external command.

### Dynamic TF-IDF Recomputation

The "dynamic segment" for a row = all included (toggled-on) commits from trunk up to that row.
Toggling a row recomputes UseTfidf names for all subsequent rows in the stack.
A row whose recomputed name is empty *or already taken* falls back to `[ ]` Unchecked —
the same skip rule the state cycle applies,
so a toggle can never park a row on a name `jj bookmark create` would reject.

### Edit Mode

Pressing `i` on a UserInput row enters edit mode (insert chars, Backspace to delete, Esc/Enter to finish).
Invalid names render in red.
The help line changes to: `Type name  Backspace delete  Esc/Enter done`.

### Context-Aware Help Line

The bookmark screen help line updates based on the currently selected row's state, adding relevant keys
(e.g., `r/R cycle` on UseExisting rows with >1 bookmark, `i edit` on UserInput rows, `r/R vary` on UseTfidf rows,
`r/R regenerate` on UseCustom rows).

### Validation on Confirm

Enter in BookmarkAssignment runs `build_result()` which validates git ref rules,
checks for duplicate names across included rows, and ensures no bookmark is still loading.
Errors display in the subtitle bar (red, bold).

### Async Bookmark Commands

UseCustom spawns a background task per row.
Uses `BookmarkNameCache` (keyed by commit IDs) with `Computing`/`Computed` entries.
Computing entries time out after 60s.
The event loop polls at 80ms while commands are in-flight to animate the spinner (10-frame animation).

## Demo recording (scripts/record-demo.py)

`mise run record-demo` regenerates the README's `media/stakk.gif`
(and `media/stakk.asciinema`):
it resets the playground repo (closes PRs, deletes branches, reseeds a demo graph),
records a scripted TUI session via asciinema-in-tmux, and converts with agg.

**The script is coupled to the user interface.**
Its choreography sends raw keys and polls for literal screen strings.
If you change any of the following, update `scripts/record-demo.py` in the same change:

- shortcut keys or navigation directions (Space/`b` cycle, `r`/`R`, `i`,
  `j`/`k`, arrow keys, Enter/Esc semantics),
- the state-cycle order or which states get skipped,
- screen titles / final output strings (`" Select branch stack"`,
  `" Assign bookmarks to commits"`, `"Submitted {n} bookmark(s)."`),
- initial row states, cursor start position, or leaf ordering on screen 1.

## Patterns & Gotchas

- `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]` for fields the tests read but production does not —
  satisfies both `--all-targets` clippy and `-D warnings`.
  It is legitimate in exactly two situations, and everything else is dead code to delete:
  1. A test reads the item as an *observable for logic that runs in production*.
     `AuthToken::source` (which token env var wins per host), `ChangeGraph`'s four construction maps
     (stacking, leaves, segment grouping, merge taint) and `Bookmark::change_id` are the surviving cases.
     `Bookmark::change_id` is what `parse_bookmarks_single` reads to pin production's `jj bookmark list` parse:
     it comes from the nested `CommitData` in `BookmarkEntryRaw::target`
     (`BOOKMARK_TEMPLATE` emits only `name`, `synced` and `target`, so there is no change-ID field to take it from).
     `Bookmark::synced` is *not* on this list — `graph::derive_remote_states` reads it in production,
     which is also what keeps `BookmarkEntryRaw::synced` read rather than suppressed.
     A stack-roots set is *not* on this list: roots are the changes absent from `adjacency_list`,
     so a field recording them is a second copy of a fact production already derives,
     and nothing but its own tests read it.
  2. The field is a serde field with *no* `#[serde(default)]`,
     so deleting it would relax the parse from "fail loudly on a template/parser mismatch" to "accept anything" —
     `CommitRefData::target` is the only one.
     (`LogEntryRaw::immutable` follows the same rule but is read in production, so it needs no suppression.)
     A serde field that *does* carry `#[serde(default)]` is already optional;
     deleting it changes no parsing and it should go.

  The `reason` must name the behaviour the test pins,
  not say "used in tests" or claim a diagnostic consumer that does not exist.
  A test that only exercises the suppressed item itself
  (a parallel implementation of production logic, a parse for a value nothing writes) justifies nothing — delete both.
  `#[expect]` warns once a suppression becomes unnecessary, so `mise run ci` catches stale ones —
  find the current set with `grep -rn dead_code src/`.
- `stakk docs` `include_str!`s `docs/*.md`,
  so those files are the single source of truth and cargo rebuilds the crate when they change (no `build.rs` needed).
  Adding a topic means a `DocTopic` variant, a `docs::source` arm, and the file —
  the index is generated from `DocTopic::value_variants()`, so it cannot drift.
  The variant doc comments are load-bearing:
  clap prints them as possible-value help *and* `docs::index` reads them back.
  `DocTopic` has no `Default` on purpose — `None` means "print the index", not "print a default topic".
  `stakk docs` is exempt from the four-touchpoint rule (§8): no env var, no config key,
  so README is the only other touchpoint.
  Add it to the `runs_jj` false-arm in `main.rs` alongside `Completions`,
  or the `_ => true` fallthrough makes it shell out to jj for a version check it does not need.
- `stakk docs` output depends on where it goes: a TTY gets prose re-flowed to the terminal width,
  a redirect gets the source verbatim.
  The verbatim path is what makes `stakk docs scripting >> AGENTS.md` reproduce the file byte for byte,
  and it is asserted by a test — do not "improve" it by always rendering.
  Width resolution checks `COLUMNS` before `console`, which reads the terminal over ioctl and never consults it;
  without that there is no way to exercise a narrow terminal.
  Fenced blocks pass through the wrapper unwrapped
  (a folded shell command is a broken one),
  so a test caps fenced lines in `docs/*.md` at 76 chars — rumdl formats at 120 and will not catch it.
- `Cli` has no flattened `SubmitArgs`: every submit flag lives on the `submit` subcommand only,
  so flags before a subcommand fail with clap's stock "unexpected argument".
  Bare `stakk` still means `stakk submit`: the `None` arm calls `cli::default_submit_args`,
  which takes the `Config` and parses the synthetic argv `["stakk", "submit"]` through a `Command` it config-applies
  itself — the signature is what stops a caller from handing it an unprepared `Command`.
  That clap parse is what makes `STAKK_*` env vars and config-injected defaults apply to the bare form —
  `SubmitArgs` deliberately has no `Default` impl, because hand-building one silently drops both
  (the bug 90718ef5cf97 fixed; `bare_stakk_submit_args_come_from_a_config_applied_clap_parse` pins the mechanism).
  Only `global = true` args (`--config`, `--github-host`) are accepted on either side of the subcommand.
- Remote host handling: `jj::remote::parse_remote_url` parses `<host>/<owner>/<repo>` for *any* host;
  `parse_github_url` layers the gate on top, accepting `GITHUB_COM` plus one configured host
  (`--github-host` / `STAKK_GITHUB_HOST` / `github_host` / `GH_HOST`, resolved once in `run()`).
  The URL is the source of truth for the host — the setting only says which hosts are allowed,
  so a repo with both a github.com and an Enterprise remote still works.
  SSH ports are dropped (they say nothing about the API) and HTTP(S) ports are kept (they are the API port).
  `GitHubRepo::api_base_uri()` returns `None` for github.com,
  so octocrab keeps its own `https://api.github.com` default, and `https://<host>/api/v3` otherwise —
  the two are not one template.
  Always `https`, even for an `http://` remote.
- The remote must be resolved *before* the token:
  `auth::resolve_token(host)` picks `GH_TOKEN`/`GITHUB_TOKEN` for github.com
  and `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` otherwise, and passes `--hostname` to `gh auth token`.
  Both pairs are in the order `gh help environment` documents,
  so stakk's fallback and gh's own answer agree on which variable wins.
  Without `--hostname`, gh answers for whatever `GH_HOST` names, which need not be this repo's host.
  `env_sources`/`token_from_env` take the lookup as a closure so tests never mutate the process environment.
- jj JSON output uses NDJSON (one JSON object per line).
  Parse with `lines()` plus a per-line `serde_json::from_str`.
- `jj git remote list` outputs plain text, not JSON.
  Parse with string splitting.
- `trunk()` remote bookmarks include an internal `@git` entry — filter it out.
- Unsynced bookmarks produce duplicate entries in `jj bookmark list` —
  deduplicate by keeping only the first entry per name.
- Graph traversal uses `"trunk()"` as the revset base, not a branch name.
- octocrab treats PR comments as issue comments — use `issues().list_comments()`.
- octocrab `pulls().create()` borrows the handler — bind to a variable first.
- Commit-derived PR body is only set on creation, not on update — avoids overwriting manually-edited PR bodies.
  Body-mode stack placement updates only the fenced section.
- `markdown::unwrap::unwrap_markdown` reflows hard-wrapped description prose into soft-wrapped paragraphs
  (structural Markdown passes through verbatim).
  It runs on *both* sides of the body-sync comparison — the generated body and the existing PR body —
  so a body GitHub stores unchanged does not look different just because of line breaks.
  Changing the reflow rules changes change-detection; the round-trip must stay idempotent.
  It is shared with `stakk docs`, which uses it as the join pass
  before `markdown::wrap::wrap_markdown` folds prose to the terminal width —
  so a change here moves rendered docs as well as PR bodies.
- Stack comment metadata line (`<!--- STAKK_STACK: ... --->`) is always prepended programmatically —
  not part of the minijinja template.
  Warning/repo-URL preambles are added per-placement-mode: comment mode uses `with_comment_preamble()`,
  body mode adds `BODY_WARNING` inside the fence.
  `format_stack_comment` itself is placement-neutral (no warning lines).
  `STAKK_REPO_URL` is the single source of truth for the repo URL.
- `format_stack_comment` returns `Result` because user templates can fail.
- `StackCommentContext.stack` is ordered trunk-first
  (`position` is 1 nearest the trunk);
  the default template reverses it in jinja
  (`stack | reverse`) so the rendered graph reads leaf-at-top like `stakk show` and the TUI.
  Reversing in Rust instead would silently flip iteration for every user template —
  keep the order change in the template.
- The default template draws one node glyph per line at column 0 — `●` current, `○` other, `◆` trunk,
  the same glyphs as `graph/layout.rs`.
  GitHub renders comments in a proportional font, so nothing may depend on horizontal alignment: no `│` gutter,
  no indentation, and no code fence (fenced links are dead).
  Lines separate on GitHub's soft-break-as-`<br>` rendering.
- Entry `title` is the *commit-derived* title,
  which can differ from the PR's live title on GitHub when `--sync-pr-content` does not include titles.
  The default template deliberately shows the bare `pr_url`
  (GitHub renders it as `#N` with the real title on hover)
  plus the bookmark name, so the comment cannot contradict the PR page.
- Body-mode fences (`STAKK_BODY_START`/`STAKK_BODY_END`) are HTML comments, invisible on GitHub.
  Migration between placement modes is automatic.
- `StackPlacement` (4 CLI modes) resolves to `EffectivePlacement`
  (4 behaviors) in `resolve_placement`: `comment` → `Comment`, `body` → `Body`, `none` → `Cleanup`, `ignore` → `Ignore`.
  Keep the two enums distinct — `Cleanup` is also reached by single-bookmark submissions,
  which have no CLI mode of their own.
- `--stack-placement none` writes no stack info and instead runs `cleanup_stack_artifacts` on every PR,
  deleting the stack comment and stripping the body fence.
  Single-bookmark submissions take the same cleanup path
  (one PR is not a stack),
  so the two share one branch that runs *before* the template and context setup that only `comment`/`body` need.
  PRs created in the same run are skipped — they cannot yet carry a stack comment.
  In `none` mode the custom `--template-path` is never read or compiled,
  so a broken template does not fail a submission that will not render it.
- `--stack-placement ignore` writes nothing *and* cleans up nothing:
  the `EffectivePlacement::Ignore` arm short-circuits before the cleanup branch,
  so it also wins over the single-bookmark cleanup rule.
  Like `none`, it never reads or compiles `--template-path`.
- ratatui inline viewport: `enable_raw_mode()` before, `disable_raw_mode()` after.
- The inline viewport is sized per screen
  (graph vs bookmark rows).
  ratatui cannot resize an inline viewport in place,
  so screen transitions erase the viewport and recreate the `Terminal` anchored at the same top row
  (`replace_viewport` in `select/app.rs`).
  On exit the viewport is collapsed (cursor to viewport top, erase down)
  and a one-line summary is printed in its place — no blank region is left behind.
- Graph layout deduplicates shared segments by `commit_id` (not `change_id`).
- Auto-generated bookmark names: `stakk-<first 12 chars of change_id>`.
- `LOG_TEMPLATE` emits an `immutable` field; `LogEntryRaw` is deliberately
  *not* serde-defaulted so a template/parser mismatch fails loudly as a
  `ParseError` instead of silently parsing `immutable: false`.
- `SegmentCommit::local_bookmark_names` carries the *unfiltered* local bookmark names from jj log —
  unlike `BookmarkSegment::bookmark_names`, it includes bookmarks the bookmarks revset excluded
  (e.g. on immutable commits).
  Feeds `excluded_bookmarks` in the graph layout, which labels locked TUI rows.
- One phase-1 constructor: every submission — the TUI and the explicit flags via `select::explicit` alike —
  goes through `analysis_from_selection(path, assignments, ...)`.
  Boundaries are matched by change ID on the selected trunk→tip path,
  so new bookmarks need not exist yet and no graph rebuild happens;
  commits between boundaries fold into the boundary above.
  Marking every boundary reproduces the no-fold shape (issue #184): each segment keeps exactly its own commit.
  Commits *above* the topmost mark are dropped from the submission entirely:
  `pending` is never flushed past the last boundary, so an unbookmarked head is not submitted unless it is marked.
- Explicit selection (`select/explicit.rs`):
  `--keep`/`--new REV[=NAME]`/`--new-auto REV`/`--new-command REV` marks fully determine the PR set —
  every PR boundary is named on the command line, nothing is implicit and there is no bulk flag.
  Revs prefix-match change/commit ids on the graph
  (deduped by commit_id: shared segments cloned into several stacks are one commit, not ambiguous);
  colinearity is validated by intersecting per-mark containing-stack sets; the topmost mark is the tip.
  `resolve_bookmarks_explicitly` requires at least one mark —
  an empty `SelectionSpec` means the TUI and `main.rs` routes it there,
  so the intersection `reduce` `expect`s a non-empty mark list.
  Errors are `stakk::selection::*` diagnostics pointing at `stakk show`.
  "Submit my whole stack" is a `stakk show --format=json` + `jq` idiom that emits one `--keep=` per *segment* —
  `bookmark_names[0]`, not `bookmark_names[]`.
  A commit can carry several bookmarks,
  and two `--keep`s on one commit are two marks on one boundary (`stakk::selection::duplicate_mark`).
  See `docs/scripting.md`.
- `stakk show`'s JSON is one schema in two projections: `--format=json` is sparse, `--format=json-full` is everything.
  Sparse must stay a *strict subset* of full — same names, types, values and emitted order.
  That is enforced by construction: full-only fields
  (`commit_id`, `description`, `author`, `files`)
  are `Option`s on the single `CommitReport` with `skip_serializing_if = "Option::is_none"`,
  so they are *omitted* rather than nulled and there is no second serializer path to drift.
  `sparse_is_a_strict_subset_of_full` pins values, `sparse_omits_full_only_fields` pins the field set on every commit
  (omitted, never nulled),
  and `sparse_field_order_matches_full` pins the emitted order per commit object — the last one reads the rendered text,
  because a parsed `serde_json::Value` sorts its keys,
  and it scans each commit object separately
  because a document-wide cursor scan accepts a permutation inside one commit by matching the later keys in the commits
  that follow.
  `show::json_projection` maps `--format` to the projection so the wiring is testable rather than inline in `main`.
  `title` is the first line of `description`; `description` stays the full message.
  Both projections report the same `SCHEMA_VERSION`.
  `committer_timestamp` is in *both* projections on purpose: stack order is derived from the committer timestamp
  (`group_segments_into_stacks`), not the author one, so without it in sparse a consumer cannot reproduce or override
  the order it is being handed.
- Each segment reports `bookmarks: [{name, remote_state}]`,
  where `remote_state` is `unpushed` / `diverged` / `synced` from `graph::derive_remote_states`.
  Two facts are needed and neither suffices alone: jj's `synced()` is false only when a *tracked* remote disagrees,
  so a never-pushed bookmark reports `synced = true` exactly like an up-to-date one.
  The tiebreaker is whether a remote bookmark of the same name sits on the segment's boundary commit.
  Match the name exactly up to the `@` — a bare prefix test calls `feat` synced when `feat-2@origin` is on the commit —
  and skip jj's internal `name@git`, which is not a push target.
  The state is offline and says nothing about pull requests, only about what a push would do.
- `excluded_bookmarks` (names) and `excluded_head_count` are separate
  because the old single counter conflated bookmarks excluded by merge taint with unbookmarked *heads* excluded the same
  way.
  Heads have no name to report, so a consumer reading only a count could not say what it lost.
- Reserved bookmark names: `Jj::get_local_bookmark_names`
  (`jj bookmark list` with *no* `-r`) is the single source of truth for "this name is taken".
  The change graph is not — it cannot see trunk's own bookmark
  (`~ trunk()` plus the `trunk()..to` traversal range),
  other people's (`mine()`), bookmarks on off-stack immutable commits,
  or anything a custom `--bookmarks-revset` filtered out.
  `main.rs` fetches the set once per selection-based `submit` and hands it to both `resolve_bookmarks_explicitly`
  and `resolve_bookmark_interactively`; both reject new names in it
  (`stakk::selection::name_exists` / `SelectionError::NameExists`)
  and skip TF-IDF/generated/custom states that would produce one.
  Kept bookmarks are exempt — reusing them is the point.
  The names-only template is parsed by `parse_bookmark_names`, not `parse_bookmarks`, which drops conflicted bookmarks
  (`target: null`) whose names are still taken.
  Deleted-but-tracked bookmarks are included for the same reason: `jj bookmark create` rejects them too.
  Untracked remote-only bookmarks (`foo@origin`, no local `foo`) do *not* block creation,
  so the query deliberately omits `--all-remotes`.
- New bookmarks are created by `execute_submission_plan`
  (`SubmissionPlan::bookmark_creations`, before the push loop),
  not at selection time — so `--dry-run` never mutates the repo
  and the plan prints `Create bookmark <name> at <short_change_id>` lines instead.
  Execute re-queries the reserved names and fails with `stakk::submit::bookmark_names_taken` *before* creating anything,
  so a name that appeared between selection and execution cannot leave a half-applied plan.
  The query is skipped when there is nothing to create.
  Accepted limitation: nothing checks a new bookmark's name against the bookmarks revset (only against existing names).
  Under a custom `--bookmarks-revset` that excludes the new name,
  the submission succeeds but subsequent runs will not see or manage that bookmark's PR.
- Stack reorder safety: bookmarks must be pushed one-at-a-time with immediate base/PR updates.
  If all bookmarks are pushed before bases are updated, a PR whose head moved down the stack will have an empty diff
  (head is ancestor of stale base), triggering GitHub auto-close.

## Key Decisions

- **No jj-stack compatibility** — own `STAKK_STACK` prefix, snake_case serde.
- **No anyhow** — concrete error types with `Diagnostic` all the way up.
- **PR title/body on creation by default; opt-in sync** — commit-derived title and body are set on PR creation.
  `--sync-pr-content` (`none`/`title`/`body`/`all`) enables updating existing PRs.
  Change detection avoids redundant API calls.
  In `--stack-placement body` mode, body sync skips the per-bookmark API call
  and lets the fence-splicing phase handle it in one update.
- **`--stack-placement none` also removes** —
  disabling stack info retires the existing artifacts rather than leaving them stale,
  so the feature can be turned off cleanly.
  Deletion is not previewed by `--dry-run`, which returns before the execute phase.
- **`ignore` exists because `none` deletes** — turning stack info off and leaving other
  tooling's (or your own) comments and body fences alone are two different wishes.
  `none` serves the first, `ignore` the second; neither is a safe default for the other.
- **`--dry-run` not in env vars** — one-off decision, surprising as a default.
- **Selection flags not in env vars/config** — `--keep`, `--new`, `--new-auto`,
  `--new-command` are per-invocation decisions like `--dry-run`;
  a persisted default would silently change what gets submitted.
  CLI + README touchpoints only (deliberate exception to the four-touchpoint rule).
- **Generic `Jj<R: JjRunner>`** — zero-cost dispatch, edition 2024 async traits.
- **Three-phase submission** — analyze → plan (queries forge) → execute.
  All repo mutations (bookmark creation, pushes) live in execute, so `--dry-run` —
  which returns after printing the plan — writes nothing.
  Analyze is not side-effect-free: a configured `--bookmark-command` runs during selection, under `--dry-run` too.
- **ratatui over inquire** — visual graph rendering, bookmark assignment TUI.
- **minijinja for stack comments** — customizable templates, metadata outside template.
- **Interleaved push+update** — `execute_submission_plan` processes each bookmark sequentially
  (push, update base, create PR) trunk-to-leaf to prevent GitHub from auto-closing PRs during stack reorders.
  Pipelining is not safe.
