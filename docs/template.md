<!--- stakk-docs
summary: Stack info placement and stack comment templates.
--->

# Stack info and templates

Where stakk writes the stack overview on each PR, and how to change what it says.

## Placement

`--stack-placement` (`stack_placement`, `STAKK_STACK_PLACEMENT`):

| Mode | Writes | Removes existing stack comments/body fences |
|------|--------|---------------------------------------------|
| `comment` | A PR comment, updated in place | When migrating from `body` |
| `body` | A fenced section in the PR body (`STAKK_BODY_START` … `STAKK_BODY_END`) | When migrating from `comment` |
| `none` | Nothing | On every submit |
| `ignore` | Nothing | Never |
| `auto-comment` (default) | Like `none` when a native stack is in effect, like `comment` otherwise | Follows the resolved mode |
| `auto-body` | Like `none` when a native stack is in effect, like `body` otherwise | Follows the resolved mode |

Content outside the body fences is preserved; the fenced section is overwritten on every run.

`none` retires stakk's stack info cleanly; `ignore` leaves whatever is on the PR frozen,
for when another tool or process owns it.
Neither reads or compiles a custom `--template-path`.

The auto modes defer to `--native-stacks`: when the server-side stack was registered on this run,
GitHub's own rendering replaces stakk's text, so they behave like `none`; otherwise like `comment`/`body`.
Retirement needs a *definitive* native stack.
If registration failed for a reason that says nothing about availability
(a network error, say),
the auto modes still write — a redundant overview self-heals on the next successful run, a skipped one goes stale.
While `--native-stacks` is `ignore` (its default) or `none`, the auto modes are exactly `comment` and `body`,
which is why `auto-comment` is the default:
the intended end state is a single default flip of `native_stacks` to `auto`,
giving native rendering where enabled and stack comments everywhere else, never both.

A submission producing a single PR is not a stack: no stack info is written, and stale artifacts from an earlier,
larger stack are cleaned up (unless the mode is `ignore`).

## Templates

Stack comments are rendered with [minijinja](https://github.com/mitsuhiko/minijinja); `--template-path <path>`
(`template_path`, `STAKK_TEMPLATE_PATH`) replaces the built-in template.

The context holds `stack`, `stack_size`, `default_branch`, `current_bookmark` and `stakk_url`.
`stack` is ordered **trunk-first** (`position` 1 nearest the trunk); the default template reverses it
(`stack | reverse`) so the result reads leaf-at-top like `stakk graph` and the TUI.
Each entry carries `bookmark_name`, `pr_url`, `pr_number`, `title`, `base`, `is_draft`, `position`,
`is_current` and `is_leaf`.
`stakk submit --help` prints the same list with a worked example.

`title` is the *commit-derived* title,
which can differ from the PR's live title when `--sync-pr-content` excludes titles.
That is why the default template shows the bare `pr_url` and no text of its own:
GitHub renders the link as a reference carrying the PR's live title and merge state,
so the comment cannot contradict the PR page.

Added outside the template, not overridable: the metadata line
(`<!--- STAKK_STACK: ... --->`),
by which stakk finds its own comment, and the placement preamble (a warning line and the repo URL).

### Rendering constraints

GitHub renders comments in a proportional font: no `│` gutter, no indentation for structure, and no code fence
(links inside a fence are dead).

**Each row must be a Markdown list item.**
GitHub expands a bare PR link into a reference with the PR's live title and merge state only inside a list item.
A link in a paragraph, table cell or blockquote stays a bare `#N` — even alone in its own paragraph.
A template that uses line breaks instead of a list still works, but every entry loses its title and state.

GitHub's bullet *is* the node marker, which is why the default template has no `●`/`○`/`◆` glyphs:
the bullet cannot be hidden (inline `style` is stripped), and a glyph next to it reads as two markers.
A glyph before the link inside the item is fine if you want one.

A template that fails to render fails the submission.
It is read and compiled before the execute phase, so a syntax error stops the run before anything is pushed;
a failure that only appears while rendering surfaces after the branches are pushed and the PRs created or updated.
`--dry-run` does not exercise the template at all.

## Custom bookmark names

`--bookmark-command` names bookmarks with an external program: `sh -c` (Unix) or `cmd /C` (Windows),
a JSON description of one segment on stdin, a single bookmark name on stdout.
It powers the `[*]` state in the TUI and the `--new-command` selection flag.
The JSON schema, with a worked example, is in `stakk submit --help`.
