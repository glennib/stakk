# Plan: v2 follow-up — documentation falsehoods and contract gaps

Status: proposed.
Seven defects from the post-v2 review — six wording, one packaging, none behavioural —
each checked against the code and, where observable, against a run of the binary.

## A — the unbookmarked tip is dropped, not folded (required)

**Defect:** `docs/scripting.md` says unbookmarked tip work folds into the top PR; it is submitted nowhere.

Mechanism: an unbookmarked segment is always the *last* in its stack,
`select/explicit.rs` passes the **full, untruncated** trunk-to-tip path,
and `analysis_from_selection` flushes its `pending` buffer only on a mark —
so everything above the topmost mark is discarded with no diagnostic.
Live in this repo: `.stacks[0].segments` is `[{v2, 11 commits}, {[], 1 commit}]`,
so the documented idiom submits `--keep=v2` and drops the working copy.
The behaviour is right — no bookmark, no branch to open a PR against — so only the prose changes.

**A1 — the Rules section**, the more important fix because it is stated as a rule.
Replace the "Bookmarks on the path that are not kept fold into the PR above them" paragraph with two:

> **Unmarked commits *below* the topmost mark fold into the PR above them.**
> The minimal selection produces the *fewest* PRs, not the most —
> a commit between two marks is absorbed into the next boundary, it does not get a PR of its own.
>
> **Commits *above* the topmost mark are not submitted at all.**
> The topmost mark is the tip, so anything newer than it — usually an unbookmarked work-in-progress head —
> is left out of the submission, silently.
> Mark it with `--new`, `--new-auto` or `--new-command` to include it.

**A2 — "Submitting a whole stack".**
Replace the "Segments with no bookmark are skipped…" sentence with:

> A segment with no bookmark is skipped, and there is at most one — the unbookmarked head, always last.
> Skipping it means those commits are **not submitted**: they sit above the topmost `--keep`,
> which puts them outside the submission rather than folding them into the PR below.
> Ask for them by name when you want them:

then a companion snippet in the same shape as the idiom above it:

```console
stakk show --format=json \
  | jq -r '.stacks[0].segments[-1]
           | select(.bookmark_names | length == 0)
           | "--new-auto=\(.commits[-1].change_id)"'
```

> Append its output — empty when the tip is already bookmarked — to the `--keep`s.

**A3 — the degenerate case**, one paragraph after the `.stacks[0]` note:

> With no bookmarked segment at all the substitution expands to nothing and `stakk submit` runs bare,
> which means the TUI — or, without a terminal, `stakk::not_interactive` and exit `1`.
> That is the correct failure for a script; test the jq output for emptiness first if you want a clearer one.

**A4 — `CLAUDE.md`** (~line 503) is not wrong, only silent on the drop;
append "Commits *above* the topmost mark are dropped from the submission entirely:
`pending` is never flushed past the last boundary, so an unbookmarked head is not submitted unless it is marked."

**A5 — `plans/cli-v2.md`** (~line 166) says unbookmarked segments "fold into the boundary above,
which is what they did under `--keep-all` too", and both halves are wrong,
since anchored `--keep-all` *expanded* along the marked path.
Amend it in place as C6 and C7 already were: the plan is the reasoning record,
and a false behavioural claim in it costs more later than the line costs now.

## B — declare `stacks[]` ordering explicitly unstable (required)

**Defect:** the README lists the `stakk show` JSON as stable without saying whether that covers array order,
and `.stacks[0]` moves as you commit anywhere in the repository.

**Recommendation: declare it explicitly *unstable***,
because declaring it stable is not a documentation change but a code commitment we would have to earn first:
`graph/mod.rs` compares committer timestamps **as strings** while jj emits RFC 3339 *with offset*,
whereas `graph/layout.rs` parses the same field with jiff,
so the JSON's `stacks[0]` and the TUI's "leaf 1" can already disagree.
Stable costs unifying both on one offset-aware comparator and freezing it, constraining all future graph-ordering work,
to serve a usage pattern the docs should be discouraging; unstable costs a caller one `jq` sort.

Two edits in README `## Stability`, phrased as a carve-out so the bullets do not read as contradicting each other.
Qualify the stable bullet:

> - The `stakk show` JSON document, under its `schema_version` — field names, types and meanings
>   (currently `2`; both the sparse `json` and the `json-full` projection report it).
>   The *order* of `stacks[]` is not part of it.

Then add to **Not stable**, carrying the actionable half:

> - The order of `stacks[]` in the `stakk show` JSON.
>   It tracks commit recency, so it moves as you commit anywhere in the repository.
>   Choose a stack by its bookmark names or its contents, never by index.

## C — `github_host` default (required)

**Defect:** the annotated `stakk.toml` in `docs/config.md` says `(default: none — only github.com is accepted)`,
but `main.rs` still consults `GH_HOST` when the key is unset, as the precedence list later in the same file,
`docs/auth.md` and `--github-host`'s help all state.
Replace the comment with:

```toml
# Extra host to treat as GitHub, for GitHub Enterprise Server
# (default: unset — falls back to GH_HOST; github.com is always accepted)
```

## D — `is_boundary` (required)

**Defect:** `docs/scripting.md` calls it "the commit its segment's bookmarks point at",
but `show/mod.rs` sets it on the newest commit of *every* segment — verified live,
where this repo's unbookmarked segment reports `bookmark_names: []` with `is_boundary: true`.
Replace the bullet with:

> - `is_boundary` — the newest commit of its segment.
>   For a bookmarked segment that is the commit the bookmarks point at; an unbookmarked head segment has one too,
>   pointing at nothing

## E — document the exit codes (required)

**Defect:** the README declares exit codes semver-guarded and no file documents them.
All four were verified by running the binary: `0` on success and on `--help`/`--version`, `1` on a stakk error,
`2` on an unknown flag, unknown subcommand or bad `docs` topic, and `130` on `Ctrl-C` at the TUI graph screen.

**Recommendation: document the table, and narrow the contract for `2`**, which is clap's code rather than ours,
so a bare "exit codes are stable" bullet silently promises clap's behaviour.
Add `## Exit codes` to `docs/scripting.md` after `## Errors`, plus a line saying `2` comes from clap,
stakk's argument parser, rather than from stakk itself:

| Code | Meaning |
|------|---------|
| `0` | Success. `--help` and `--version` also exit `0` |
| `1` | stakk failed; the diagnostic, with its `stakk::…` code, is on stderr |
| `2` | Usage error — unknown flag, unknown subcommand, invalid enum value |
| `130` | Interrupted (`Ctrl-C` in the TUI) |

Then replace the README's bare `- Exit codes.` with the values it actually pins — contract in the README,
reference table in a doc topic, the split diagnostic codes already use,
and the "doc text is unstable" clause covers the *wording* rather than the values:

> - Exit codes: `0` success, `1` failure, `130` interrupted.
>   `2` is clap's usage-error convention and follows clap, not this contract.
>   The table is in `stakk docs scripting`.

## F — stop publishing internal files to crates.io (required)

**Defect:** `cargo package --list` ships `plans/`, `CLAUDE.md`, `scripts/record-demo.py` and `.github/`.
Add `exclude = ["plans/", "scripts/", ".github/", "CLAUDE.md"]` to `Cargo.toml` — `exclude` rather than `include`,
since the two are mutually exclusive and a whitelist would eventually drop something the build needs unnoticed.
Verified: nothing `include_str!`s from `plans/`
(only `docs/*.md` and `src/forge/default_comment.md.jinja` are compiled in),
so `docs/` must stay packaged and this list leaves it alone; `media/` also stays,
because the README references `media/stakk.gif` relatively
and how crates.io rewrites relative links is not worth guessing at for 628 KB; and release-plz owns only `version`,
so an `exclude` key does not collide with it.

## G — `--dry-run` is not "fully inert" (required)

**Defect:** `docs/scripting.md` calls `--dry-run` "fully inert" and "always safe",
but a configured `--bookmark-command` runs during phase 1, before the `dry_run` early return.

Verified control flow: `main.rs` resolves the selection in phase 1, calls `create_submission_plan` —
which *reads* the forge — in phase 2, and only then returns on `args.dry_run`,
while `select/bookmark_gen.rs` executes the configured command through `sh -c`.
So `stakk submit --dry-run --new-command <rev>` runs it, and so does cycling a TUI row to `[*]custom`.
The enumerated guarantees hold; only the blanket claim fails, and "without touching GitHub" is loose besides.
Replace the `## Dry runs` opening with:

> `--dry-run` prints the planned bookmark creations and PR actions, then stops.
> It creates no bookmark, pushes nothing and makes no write to GitHub — though the plan phase does *read* GitHub,
> to find the pull requests that already exist.
> One thing does run: a configured `--bookmark-command` is executed during selection, before the plan is printed,
> both for `--new-command` marks and for TUI rows cycled to `[*]custom`.

The `--dry-run` doc comment in `src/cli/submit.rs` leads with the same claim, and its enumeration is fine,
so only the lead changes:

```rust
/// Print the submission plan and stop.
///
/// No bookmark is created, nothing is pushed, and no pull request is
/// touched — but a configured --bookmark-command still runs during
/// selection.
```

Two more places need the same treatment: `README.md` (~line 32, "without touching GitHub — or the repo") and `CLAUDE.md`
(~line 588, where "analyze (pure)" is wrong for the same reason as "is fully inert").
`README.md`'s two "re-running `stakk submit` is always safe" lines are about idempotency rather than dry-run
and should be left alone.
`plans/cli-v2.md`'s `# inert plan preview` is fixed in the same edit as A5 — that file is already open,
and the word is wrong there for exactly the reason it is wrong everywhere else.

## Grouping and non-goals

Four commits, of which the first two may be collapsed if one changelog entry is preferred.
G stays separate because it is the only item that edits a `--help` string —
a surface commit 2 is simultaneously declaring semver-stable — and F because it is the only change to what ships:

1. `docs: correct what the v2 docs say about dropped tips, is_boundary, and github_host` — A1–A5, C and D, all the
   same defect: a sentence that does not match the code.
2. `docs: document exit codes and declare stacks[] ordering unstable` — B and E, which both edit README `## Stability`
   and both add reference material to `docs/scripting.md`.
3. `docs: stop calling --dry-run fully inert` — G, the one item that also edits a `--help` string.
4. `chore: stop packaging internal planning and automation files` — F.

Two things deliberately **not** done: teaching the `--keep` idiom to detect the tip itself,
since a `bookmark_names | length == 0` test inside the substitution turns a readable one-liner into something nobody
will adapt; and making `stacks[]` ordering stable, which means unifying two ordering implementations and freezing the
result to serve indexing the docs should be discouraging.
