# Plan: repo-level `stakk.toml` and the trust it should not carry

Status: **decided against for v2.0.0 — option (a), do nothing.**
The recommendation below (option (b)) was not adopted; the exposure argument in **Threat calibration** carried.
Revisit on any of the triggers named under **Recommendation**: a third dangerous key, a hook or plugin mechanism,
or a report of a `stakk.toml` in the wild that a user did not write.
Still outstanding from the (a) branch:
the "repo config runs with your privileges" note `docs/config.md` owes either way,
which is a semver-free change available in any release.

Pre-existing since repo config discovery landed — v2 introduces none of it. v2.0.0 is the deadline, not the cause:
the new **Stability** section in `README.md` declares "Config file keys and their defaults" semver-stable,
so whatever semantics repo config has when v2.0.0 ships is the semantics we owe users until v3.

## What was verified

`discover_repo_config` walks from the cwd up to the `.jj/` boundary and loads the first `stakk.toml` it finds,
with no trust step, and its values are injected as clap defaults.
A `stakk.toml` **committed into the repository** is therefore auto-loaded on clone.

- **`bookmark_command` — arbitrary code execution, demonstrated.**
  In a scratch clone with a checked-in `stakk.toml`,
  `stakk submit --dry-run --new-command <rev>` wrote the attacker's file and *then* failed on `Bad credentials`.
  The command runs in phase 1 (`main.rs:167`/`175` → `select/bookmark_gen.rs` → `Command::new("sh").args(["-c", …])`),
  before `create_submission_plan` — so it fires with a bogus token, no network, and no PR.
  The interactive route needs no flag: the `[*]custom` state is offered whenever a bookmark command is configured,
  so bare `stakk` plus a few presses of Space reaches the same `sh -c`.
- **`template_path` — file exfiltration into a public PR, traced but not executed.** `main.rs:237` does `read_to_string`
  on any path and hands it to `build_comment_env`, which `add_template`s it verbatim; minijinja renders delimiter-free
  input byte for byte, and the result becomes the PR comment or body.
  This sits *after* the `if args.dry_run { return }` at `main.rs:230`, so unlike the RCE it needs a real submit:
  valid token, network, a multi-bookmark stack, `comment`/`body` placement — but no keypress beyond an ordinary one.

Not problems, checked: `stakk show` takes only the graph defaults, so offline discovery is unaffected;
and `Config` has no `config` field under `deny_unknown_fields`, so a repo `stakk.toml` cannot chain-load another file.

## Blast radius — every `Config` field

| Key | Class | Why |
| --- | --- | --- |
| `bookmark_command` | **dangerous — executes** | `sh -c` / `cmd /C`, phase 1, no token required |
| `template_path` | **dangerous — reads any path, sends outward** | `read_to_string` + verbatim render into a PR |
| `github_host` | moderate | widens the accepted-host gate, which decides which token pair is resolved. Needs a remote on that host, and a clone gives you exactly the one remote you chose — a repo cannot add a second — so not self-sufficient |
| `remote` | inert-ish | must name a remote already in `jj git remote list`; otherwise `RemoteNotFound`. Cannot introduce a destination |
| `bookmarks_revset`, `heads_revset` | inert | jj revsets reach `jj` through `Command::new("jj").args(args)` in `jj/runner.rs` — argv, never a shell. Pure queries; worst case they widen or narrow what the TUI offers |
| `inherit` | inert, but amplifying | repo-controlled directive that can suppress the user config, so any guard keyed off user config must not be reachable through it |
| `pr_mode`, `stack_placement`, `sync_pr_content`, `trailers` | inert | closed enums |
| `auto_prefix` | inert | validated as part of a bookmark name |

Only two keys need a policy, and the rule that produced the list is the durable part: a key is dangerous if it executes,
opens a path, or moves bytes outward.

## Threat calibration

The exposed population is small: stakk's normal user runs it in a repo they have commits in and push rights to.
The realistic shape is fork-and-contribute — clone an unfamiliar repo, write a commit, run `stakk` to open the PR —
and in it the victim runs stakk *before* trusting the code, which is precisely the gap.

Against that, running stakk is a **weaker** trust signal than building:
`cargo build` compiles and runs the repo's own code by design,
while `stakk` claims only to read jj state and talk to GitHub.
A user who has deliberately not built has not opted into code execution, and stakk gives it to them anyway.

Of the four precedents, **git's is the one that fits**: git deliberately does not transfer `.git/config` on clone,
for exactly this reason — a cloned repo must not be able to set executable config. stakk sits in git's seat,
and its repo config is the thing git refused to ship. direnv's `allow` is the right *shape*
for a much larger surface than two keys;
VS Code's workspace trust is a retrofit for a tool whose core job is opening untrusted code, which stakk's is not;
cargo's is the one to reject, since building is unambiguously running the repo's code and opening a PR is not.

## The legitimate case

`docs/config.md` currently sells the thing being restricted: repo config is auto-discovered with no trust step,
the `inherit = false` section offers it as a way to "enforce team-wide settings",
and the annotated example block lists both dangerous keys.
A team wanting one shared PR-comment template or bookmark-naming command in the repo is a reasonable wish,
and every option below except (a) takes it away —
because stakk cannot both let a repo dictate execution and be safe to run in a repo you have not read.

## Options

| | Stops the attack? | Cost to a legitimate user | Breaking? |
| --- | --- | --- | --- |
| **(a) Do nothing, document it** | no | none | no |
| **(b) Repo config may declare but not activate the two keys** | yes | one-time copy into user config, or a flag | yes — a declared key stops taking effect |
| **(c) direnv-style allowlist (path + content hash)** | yes | an `allow` step per repo, re-run on every edit; new state file, new subcommand | yes |
| **(d) Prompt on first use** | mostly | a prompt; breaks non-interactive use, and the RCE path is a TUI already primed to accept keypresses | yes |
| **(e) Warn loudly, proceed** | no | noise on every run | no |
| **(f) Restrict `template_path` to inside the repo** | partially | none | yes, narrowly |

(b) is cheap because the payload is two keys,
not a config format. (c) is the general answer and would be right if the dangerous surface kept growing;
today it is a state file, a subcommand
and a re-`allow` on every edit to buy what (b) buys with a load-time filter. (f) needs an honest caveat:
it blocks `~/.config/gh/hosts.yml` and `~/.ssh/`, but a `.env` in the working copy is *inside* the repo — a reduction,
not a fix.

## Recommendation

**Adopt (b) for both keys, in v2.0.0, in the refined form: a repo `stakk.toml` may *declare* `bookmark_command` and
`template_path`, but they take effect only from user config, environment, or CLI.**

Refined, because plain (b) — ignore them silently — throws away the team case `docs/config.md` promises.
Instead stakk keeps the declared value, does not use it, and says so once:
`this repo suggests bookmark_command = "…"; add it to your user config or pass --bookmark-command to use it.` Team
intent survives as a discoverable suggestion, the trust transfer does not, and the `[*]custom` TUI state is simply not
offered — already the modelled state for "no bookmark command".

Timing carries this, not the size of the threat.
`Cargo.toml` is still `1.25.0` and v2.0.0 is unreleased,
so changing what a key means from a given source is free this week and costs a v3 later.
Tighten now, loosen later: relaxing to (f) for `template_path` is **not** breaking and can ship in any 2.x
if the ergonomics prove painful, whereas the reverse is not available.

If the answer is instead **(a) do nothing**, that is defensible on exposure alone, with an explicit trigger to revisit:
a *third* dangerous key, a hook/plugin mechanism, or a report of a `stakk.toml` in the wild a user did not write.
What (a) must not do is stay silent — `docs/config.md` owes a "repo config runs with your privileges" note either way,
and that note is a semver-free change available at any time.

## If adopted — the details that decide correctness

- **Strip before the early return.** `Config::load` returns `repo_config` directly when `inherit = false`,
  and `inherit` is repo-controlled — a guard on the merge path alone is disabled by one attacker-written line.
  Move the two keys into a `suggestions` carrier *immediately after* `load_from`, before the `inherit` branch.
- **Provenance must survive `merge`.** `Config::merge` flattens source, so a per-field source tag would thread through
  every field; stripping at load avoids that, and `apply_submit_defaults` needs no change —
  the stripped field is `None`, so its `set_default` simply never fires.
- **The notice needs its own path.** `main.rs` reads `args.bookmark_command`, never the `Config`,
  and a stripped key reaches `SubmitArgs` as `None` by construction.
  The carrier therefore rides beside clap: `config` already lives in `run()` (`main.rs:52`),
  so it is a new parameter on `submit_bookmark` plus the print — a signature change, not one line.
- **Say it in the contract.**
  The README's Stability list covers config *keys and defaults* and is silent on *sources*;
  add a sentence declaring which keys a repo-level `stakk.toml` may activate, rather than folding it in silently.

Size: roughly a day — `src/config/mod.rs`
(the strip, the carrier, tests for both `inherit` paths),
the `submit_bookmark` signature and the notice in `main.rs`, `docs/config.md`
(the two key comments, a trust section, a correction to the `inherit = false` framing),
the README sentence, and a `CLAUDE.md` §8 note naming these two keys as the exception.

## Considered and rejected

- **A `trusted = true` key inside `stakk.toml`** — self-certification; the attacker writes the file.
- **Gating on whether `stakk.toml` is tracked by jj** — inverted.
  A *committed* file is the dangerous one; an untracked local file is the user's own.
- **Sandboxing `bookmark_command`** — a naming helper legitimately reads the repo and may call `jj`;
  no sandbox permits that and stops exfiltration.
- **Dropping repo-level discovery entirely** — kills every inert key's legitimate use to fix two.
- **Applying the same rule to `github_host`** — it cannot act without a remote on that host,
  and a clone supplies only the one remote the user chose; restricting it would break the Enterprise case for no gain.
