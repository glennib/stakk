# Plan: pin the two v2 claims that nothing crosses `main.rs` to check

Status: implemented in `test: pin the wiring that selects a JSON projection and asks gh for a host`.
Two gaps found by mutation-testing the shipped v2 series — not a strategy revision.

## The finding, reproduced

The v2 unit tests are strong — 11 of 11 mutations were caught by their own named tests — but two are not,
and both sit past the `src/main.rs` boundary, where no test reaches and `gh` is never mocked.

| Mutation applied to the tree | Result |
| --- | --- |
| `main.rs` `show_status`: hardcode `render_json(&data, JsonProjection::Full)` | 469/469 pass |
| `auth.rs` `resolve_token`: read the env vars before asking `gh` | 469/469 pass |
| `auth.rs` `try_gh_cli`: drop `--hostname` from the argv | 469/469 pass (plus a stray `unused variable: host`) |

insta cannot help: the `json_*_snapshot` tests pass an explicit projection, pinning shape, never wiring.
`json_projection`'s doc comment stops one call short: "…rather than inline in `main` so it is testable."

## G1 — move the format dispatch into `show/`

`show/` already returns `String` and only `print!` is IO, so the dispatch needs no seam — it needs to move.
Add `pub fn render(data: &ShowData, format: ShowFormat, colors: bool) -> String` holding the `json_projection` match
and privatise `json_projection`/`render_json`/`render_pretty`; `main.rs`'s `match` becomes one `print!`.

**The test** adds `fn sample_json_via_format(format: ShowFormat)` beside `sample_json(projection)`,
calling `render(&data, format, false)` and asserting the sparse commit's key set is *exactly* C9's seven fields.
The acceptance rule is mechanical: **a test mentioning `JsonProjection` does not close the gap**,
since the fix's whole content is that the assertion routes through a `ShowFormat` value.
**Kills:** hardcoding `JsonProjection::Full` — the sparse key set gains `commit_id`, `description`, `author`, `files`.

Not a snapshot: a new `.snap` needs owner acceptance,
and the existing snapshots' expression strings sit in unchanged test bodies — **this design changes no snapshots**.

The new shape is better, not merely more testable:
`show/` exposes one entry point instead of three functions plus a projection enum
that only `main.rs` was trusted to combine correctly.

## G2 — a `GhRunner` trait at the argv level

The obvious trait — one method meaning "get a token for this host" — is the wrong shape,
because it hides the argv construction inside the real implementation and that construction *is* the second mutation.
The seam has to sit where `JjRunner` sits.

```rust
pub trait GhRunner: Send + Sync {
    fn run_gh(&self, args: &[&str])
        -> impl Future<Output = Result<GhOutput, std::io::Error>> + Send;
}
```

`GhOutput { success: bool, stdout: String }`, plus a `RealGhRunner` wrapping today's `tokio::process::Command` call.
`try_gh_cli` and `resolve_token` go generic over the runner, taking `resolve_token`'s existing env-lookup closure.
`resolve_token(host)` remains a one-line wrapper binding the real runner, so **`main.rs` is untouched**.
The mock is a `RecordingGhRunner` recording argv into an `Arc<Mutex<Vec<Vec<String>>>>`, the shape `MockForge` uses.

**Test 1 — gh wins over the environment.**
Mock gh returns `"gh-token\n"` *and* the env lookup returns `Some("env-token")`;
assert `token == "gh-token"` and `source == TokenSource::GitHubCli`.
Both-populated is load-bearing: with the env empty the test passes under the mutation, pinning nothing.
**Kills:** reordering `resolve_token` to read the env vars first.

**Test 2 — the fallback direction.**
Mock gh exits non-zero, env returns a token; assert that token and its env `TokenSource`, pinning gh-first.

**Test 3 — the argv.**
Resolve for `github.example.com`; assert the recorded argv is exactly
`["auth", "token", "--hostname", "github.example.com"]` — no assertion on any return value.
The recording runner is the observation, capturing the slice at the boundary the mutation edits.
**Kills:** dropping `--hostname`.

`try_gh_cli`'s comment says gh "answers for whichever host `GH_HOST` or its config names,
which need not be the host this repo's remote points at",
so dropping it sends an Enterprise token to `api.github.com` whenever the user's `gh` default host is their Enterprise
server — a credential crossing hosts, silently, on a green suite.

## Not covered, deliberately

- **The spinner suppression** in `show_status` — format-dependent and unpinned, but cosmetic and not one of the claims.
- **The rest of `main.rs`** — `resolve_github_remote`, `runs_jj`, the synthetic parse and template loading each want
  their own seam; none has a mutation on record.
- **The process environment** — both G2 tests drive `auth.rs`'s injected lookup closure; nothing mutates process env.

## Considered and rejected

- **A fake `gh` earlier on `PATH`, in an integration test.**
  Decisive: C6 removed `stakk auth test`, so no command resolves a token without a jj repo,
  a GitHub remote and a live API call — the black-box entrance is gone;
  it also needs the forbidden process-env mutation.
- **A `#[cfg(test)]` seam** — the path under test is not the shipped path, and it observes argv no better.
- **A pure `gh_auth_token_args(host)` builder plus a precedence combinator** — the cheap tier (~25 lines, no trait),
  which kills both mutations but verifies the argv the *builder* returns, leaving the mutated `.args(...)` unchecked.
- **`src/lib.rs` plus `tests/`** — stakk publishes as a binary, so a lib target would make every module a public
  Rust API, which C8's contract deliberately does not cover.

## Cost and recommendation

| | Files | Production | Tests | Shape |
| --- | --- | --- | --- | --- |
| G1 | `src/show/mod.rs`, `src/main.rs` | ~15 moved, 4 deleted | ~20 | Better boundary |
| G2 | `src/auth.rs` | ~35 (trait, real impl, generics) | ~60 | More testable; otherwise neutral |

No snapshots, no dependencies, no `docs/` touchpoints — neither gap adds a flag, env var or config key.

Close both, for different reasons: G1 is nearly free, and the extraction would be defensible without the test at all.
G2 costs more and does not improve the module's shape:
a trait plus a generic to make one subprocess observable is more testable, not better designed,
and this plan should not pretend otherwise.
Close it anyway: an Enterprise token reaching `api.github.com` is not a class where "no test caught it" is defensible.
