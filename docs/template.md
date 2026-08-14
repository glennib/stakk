<!--- stakk-docs
summary: Stack info placement and stack comment templates.
--->

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
Neither mode reads or compiles a custom `--template-path`,
so a broken template cannot fail a submission that will not render it.

A submission that produces a single PR is not a stack: no stack info is written, and stale artifacts from an earlier,
larger stack are cleaned up (unless the mode is `ignore`).

## Templates

Stack comments are rendered with [minijinja](https://github.com/mitsuhiko/minijinja).
`--template-path <path>` (or `template_path` in a config file, or `STAKK_TEMPLATE_PATH`) replaces the built-in template.

The context holds `stack`, `stack_size`, `default_branch`, `current_bookmark` and `stakk_url`.
The `stack` array is ordered **trunk-first** — `position` is 1 for the entry nearest the trunk.
The default template reverses it (`stack | reverse`) so the rendered graph reads leaf-at-top,
matching `stakk graph` and the TUI.

Each entry carries `bookmark_name`, `pr_url`, `pr_number`, `title`, `base`, `is_draft`, `position` and `is_current`.
`stakk submit --help` prints the same list with a worked example template.
Note that `title` is the *commit-derived* title,
which can differ from the PR's live title on GitHub when `--sync-pr-content` does not include titles.
The default template deliberately shows the bare `pr_url` and no link text of its own,
so the comment cannot contradict the PR page:
GitHub renders the link as a reference carrying the PR's live title and merge state.
The bookmark name is not written next to it — the reference is the whole label.

Two things are added outside the template and cannot be overridden:

- the metadata line (`<!--- STAKK_STACK: ... --->`), which is how stakk finds and updates its own comment
- the placement preamble — a warning line, and the repo URL

### Rendering constraints

GitHub renders comments in a proportional font, so a template must not depend on horizontal alignment: no `│` gutter,
no indentation for structure, and no code fence (links inside a fence are dead).
Each row of the default template is a **Markdown list item**, and that is functional rather than decorative.
GitHub expands a bare PR link into a reference showing the PR's live title and merge state only
when the link sits inside a list item.
A link in a plain paragraph, a table cell or a blockquote stays a bare `#N`,
and so does a link alone in its own paragraph.
A custom template that lays entries out with line breaks instead of a list still works,
but every entry loses its title and state.

The bullet GitHub draws for each item *is* the node marker,
which is why the comment carries no `●`/`○`/`◆` glyphs of its own even though `stakk graph` and the TUI do:
a glyph next to a bullet reads as two markers, and the bullet cannot be turned off
(GitHub's sanitizer strips inline `style`).
A glyph *is* allowed before the link inside an item if a custom template wants one.

A template that fails to render fails the submission, and `--dry-run` does not exercise it:
dry-run returns before the template is read at all.
What the ordering does guarantee is that a template is read and compiled before the execute phase,
so a syntax error stops the run before anything is pushed.
A failure that only appears while rendering surfaces later —
the branches are pushed and the PRs created or updated first, and the stack comments written after that.

## Custom bookmark names

`--bookmark-command` names bookmarks with an external program.
The command runs through `sh -c` (Unix) or `cmd /C` (Windows),
receives a JSON description of one segment of commits on stdin, and must print a single bookmark name on stdout.

It powers the `[*]` state in the TUI and the `--new-command` selection flag.
The full JSON schema, with a worked example, is in `stakk submit --help`.
