# Stack info and templates

Where stakk writes the stack overview on each PR, and how to customize what it says.

## Placement

`--stack-placement` (or `stack_placement` in a config file, or `STAKK_STACK_PLACEMENT`) decides
where the stack overview lives on each PR:

| Mode | Writes | Removes existing stack comments/body fences |
|------|--------|---------------------------------------------|
| `comment` (default) | A separate PR comment, updated in place | Yes, when migrating from `body` |
| `body` | A fenced section in the PR body (`STAKK_BODY_START` … `STAKK_BODY_END`) | Yes, when migrating from `comment` |
| `none` | Nothing | Yes, on every submit |
| `ignore` | Nothing | No — existing artifacts are left exactly as they are |

Switching between `comment` and `body` migrates automatically.
Content you write outside the body fences is preserved; the fenced section itself is overwritten on every run.

`none` and `ignore` both write no stack info — the difference is what happens to whatever is already on the PR.
Use `none` to retire stakk's stack comments cleanly (for example when moving to GitHub's own stacked-PR UI).
Use `ignore` to leave the existing comments and fences frozen in place — handy while trying another tool,
or when another process owns that part of the PR.
Neither mode reads or compiles a custom `--template`,
so a broken template cannot fail a submission that will not render it.

A submission that produces a single PR is not a stack: no stack info is written, and stale artifacts from an earlier,
larger stack are cleaned up (unless the mode is `ignore`).

## Templates

Stack comments are rendered with [minijinja](https://github.com/mitsuhiko/minijinja).
`--template <path>` (or `template` in a config file, or `STAKK_TEMPLATE`) replaces the built-in template.

The template receives a context with a `stack` array, ordered **trunk-first** —
`position` is 1 for the entry nearest the trunk.
The default template reverses it (`stack | reverse`) so the rendered graph reads leaf-at-top,
matching `stakk show` and the TUI.

Each entry carries its `position`, `bookmark`, `pr_url`, `title`, and whether it is the PR currently being rendered.
Note that `title` is the *commit-derived* title,
which can differ from the PR's live title on GitHub when `--sync-pr-content` does not include titles.
The default template deliberately shows the bare `pr_url` — GitHub renders it as `#N` with the real title on hover —
so the comment cannot contradict the PR page.

Two things are added outside the template and cannot be overridden:

- the metadata line (`<!--- STAKK_STACK: ... --->`), which is how stakk finds and updates its own comment
- the placement preamble — a warning line, and the repo URL

### Rendering constraints

GitHub renders comments in a proportional font, so a template must not depend on horizontal alignment: no `│` gutter,
no indentation for structure, and no code fence (links inside a fence are dead).
The default template draws one node glyph per line at column 0 — `●` current, `○` other, `◆` trunk —
the same glyphs `stakk show` uses, and relies on GitHub rendering soft breaks as `<br>`.

A template that fails to render fails the submission, so test changes with `--dry-run` against a real stack first.

## Custom bookmark names

`--bookmark-command` names bookmarks with an external program.
The command runs through `sh -c` (Unix) or `cmd /C` (Windows),
receives a JSON description of one segment of commits on stdin, and must print a single bookmark name on stdout.

It powers the `[*]` state in the TUI and the `--new-command` selection flag.
The full JSON schema, with a worked example, is in `stakk submit --help`.
