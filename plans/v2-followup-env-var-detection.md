# Plan: detect removed `STAKK_*` environment variables

Status: implemented in `feat: warn when a removed STAKK_ environment variable is still set`.
Follow-up to [`cli-v2.md`](cli-v2.md) C4 (`--draft`/`STAKK_DRAFT` removed) and C5
(`--template`/`STAKK_TEMPLATE` renamed to `--template-path`/`STAKK_TEMPLATE_PATH`).

## The asymmetry

v2 removes two configuration surfaces per option, and only one of them fails loudly.

`Config` carries `#[serde(deny_unknown_fields)]`, so a stale TOML key errors and enumerates the valid ones:

```text
× failed to parse config file stakk.toml
╰─▶ unknown field `draft`, expected one of `inherit`, `remote`, `github_host`, `pr_mode`,
    `template_path`, …
```

The environment half has no such backstop.
`STAKK_DRAFT=1` in a shell profile is read by nothing and reported by nothing.

The two silent failures are not equally bad, and the difference is the argument for acting at all:

- **`STAKK_TEMPLATE` is reversible.**
  The stack comment reverts to the built-in template and is overwritten on every PR in the stack — annoying,
  but setting `STAKK_TEMPLATE_PATH` and re-running rewrites it correctly.
- **`STAKK_DRAFT` is not.** `is_draft: plan.pr_mode == PrMode::Draft && bp.needs_create` means draft-ness applies at PR
  *creation* only, so the blast radius is the PRs created by the first post-upgrade submit — opened ready-for-review,
  reviewers notified.
  Converting them back to draft does not un-notify anyone.

Existing PRs are untouched in both cases.

## Options

- **(a) Do nothing; rely on the changelog and migration table.** Defensible for `STAKK_TEMPLATE`.
  Not for `STAKK_DRAFT`: the failure is silent, irreversible, and visible to other people.
  It is also inconsistent — we already decided the config half should fail loudly.
- **(b) Warn on stderr when a removed `STAKK_*` variable is set.** Recommended; see below.
- **(c) Hard error.**
  Rejected: stakk cannot know the variable is *its* variable rather than something an unrelated tool exports,
  and refusing to run over a variable we no longer own is worse than the break it guards.
  It would also block `stakk show`, which is the discovery command a confused user reaches for.
- **(d) Warn for a window, then delete.**
  This is (b) plus a removal trigger, which the plan states below — not a separate option.
- **(e) Keep the old names working as hidden aliases**
  (`STAKK_TEMPLATE` as a fallback for `STAKK_TEMPLATE_PATH`, a hidden `--draft`).
  Rejected on C3's own reasoning: keeping a surface and removing it later needs another major,
  while removing now and re-adding later is free.
  It also defeats C5 — the point of the rename was that `template` reads as inline content.
- **(f) A general deprecation mechanism** (macro, clap shim args, a `Deprecated` trait).
  Rejected as over-engineering: a static table of `(removed_name, advice)` pairs *is* the general mechanism,
  and it extends by one line.

## What is watched

Exactly two names: `STAKK_DRAFT` and `STAKK_TEMPLATE`.

That set is complete, and cheap to verify without git history:
CLAUDE.md §8 forces every user-facing environment variable into `docs/config.md`'s **Environment variables** table,
so that table plus `src/cli/` is the authoritative present set, and `CHANGELOG.md` names the only two that ever left it.
Nothing else was ever removed or renamed — `--keep-all` and `stakk auth`
(C3, C6)
had no environment variables, by the same deliberate rule that keeps `--dry-run`
and the selection flags out of the environment.

`STAKK_STACK`, `STAKK_REPO_URL` and `STAKK_BODY_START`/`STAKK_BODY_END` are comment markers and template context,
not environment variables, and are out of scope.

Removed *flags* need no help: clap already errors on `--draft` with "unexpected argument".
Removed *config keys* need no help: `deny_unknown_fields` covers them.
The environment is the only unguarded third.

## Where the check lives

In `src/config/mod.rs`, next to `pre_parse_config_path` — the one place that already reads the environment directly,
rather than a new module for forty lines.

`run()` calls it after `Cli::from_arg_matches`, so `--help`,
`--version` and parse errors exit first without a stray warning attached,
and gates it the way the existing `runs_jj` match gates the jj version check:
warn on `Some(Commands::Submit(_))` and `None`, the two paths that consume submit args.
`stakk show`, `docs` and `completions` never reach it, so the cost in the repo-free paths is zero —
no environment reads, no stderr line to corrupt a shell that `eval`s `stakk completions zsh`.

Considered and rejected: mirroring `runs_jj` exactly (warn for everything except `completions`/`docs`).
It reuses an idiom already in `main.rs` and costs `stakk show` two environment lookups,
but neither removed variable ever affected `show`, so it would warn about something that changed nothing.

Also rejected: moving the check into `submit/` so it can print next to the plan.
It puts temporary deprecation logic inside the submission pipeline, which then has to be surgically removed later.

**Accepted limitation, stated plainly.**
A line printed before the spinner scrolls past above the plan and the execute output,
which is where attention actually is — so the user this exists for may still not read it.
It survives redirection (`stakk submit > log` keeps stderr on the terminal) and it is one line rather than a wall,
which is the trade we accept: correct placement would cost the coupling above, for a feature that is meant to die.

## Silencing

There is no silencer flag and no silencer variable.
`STAKK_NO_DEPRECATION_WARNINGS` would be stable surface under the README's own "`STAKK_`-prefixed environment variables"
bullet, so adding one to soften a break would create a *new* variable we could not remove without another break.

The answer to "how do I silence it" is "unset the variable", which is also the fix.
As a consequence of ordinary hygiene, the check treats an unset *or empty* variable as absent —
matching `main.rs`'s existing `filter(|h| !h.is_empty())` for `GH_HOST` —
so a CI job that cannot drop the export can set it empty.

## Stability contract

The README's **Not stable** list covers spinner text and error *wording*.
An advisory warning is neither, so it currently sits in a gap.
One bullet closes it:

> - Advisory warnings printed to stderr.
>   They may be added or removed in any release.

Worth adding independently of this plan: it retroactively covers `warn_if_jj_too_old`,
which today is excused only implicitly, by the jj-floor line.

That bullet is also what makes this deletable.
**Removal trigger:** the check goes at v3.0.0 at the latest, and may go in any 2.x once the v1 population has moved —
no major required, because the warning was never stable surface.

## Testability

A pure function, `removed_env_vars(lookup: impl Fn(&str) -> Option<String>) -> Vec<&'static RemovedVar>`,
taking the environment lookup as a closure exactly as `auth::token_from_env` does;
`main.rs` passes `|name| std::env::var(name).ok()`.
No process-environment mutation in tests.

Tests assert on the *matched entry* — the variable name and its replacement — never on the rendered message,
the way `auth.rs` asserts on `TokenSource` rather than on the token.
Asserting on the prose would contradict the contract clause added above and break on every reword.

## Recommendation

Ship (b).
The deciding fact is not symmetry with the TOML half but `STAKK_DRAFT`'s irreversibility:
every other silent v2 break costs a re-run, and this one costs a notification to other people that cannot be recalled.
A warning is the cheapest instrument that addresses it, it is precedented
(`warn_if_jj_too_old` is the same shape), and it is the only option on the list that costs nothing to remove later.

Not part of the stable surface, so no follow-up break.
`stakk docs` and `completions` are unaffected. §8's four-touchpoint rule does not apply:
there is no flag and no config key here, the same exemption C9 claimed for `--format`.

**Size:** roughly 80 lines across four files — `src/config/mod.rs`
(the table, the function, its tests, ~60),
`src/main.rs` (the gate and call, ~6), `README.md` (one Stability bullet), `docs/config.md`
(a short note under the Environment variables table).
One `feat:` commit; not a breaking change.
