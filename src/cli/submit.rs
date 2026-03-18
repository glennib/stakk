use clap::Args;

use crate::cli::graph::GraphArgs;
use crate::forge::comment::StackPlacement;

/// Arguments for the `submit` subcommand.
#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The bookmark to submit as a pull request. If omitted, shows an
    /// interactive selection.
    pub bookmark: Option<String>,

    /// Show what would be done without actually doing it.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub graph: GraphArgs,

    /// Create pull requests as drafts.
    #[arg(long, env = "STAKK_DRAFT")]
    pub draft: bool,

    /// Git remote to push to.
    #[arg(long, default_value = "origin", env = "STAKK_REMOTE")]
    pub remote: String,

    /// Path to a custom minijinja template for stack comments.
    ///
    /// The template is rendered with minijinja and receives the following
    /// context:
    ///
    ///   stack             — list of entries (see below)
    ///   stack_size        — total number of entries
    ///   default_branch    — name of the trunk branch (e.g. "main")
    ///   current_bookmark  — the bookmark being submitted
    ///   stakk_url         — URL to the stakk project
    ///
    /// Each entry in stack has:
    ///
    ///   bookmark_name  — bookmark name
    ///   pr_url         — full URL to the pull request
    ///   pr_number      — PR number
    ///   title          — PR title
    ///   base           — base branch name
    ///   is_draft       — whether the PR is a draft
    ///   position       — 1-based position in the stack
    ///   is_current     — true for the PR being submitted
    ///
    /// Example template:
    ///
    ///  Stack ({{ stack_size }} PRs, merges into `{{ default_branch }}`):
    ///   {% for entry in stack %}
    ///   - {{ entry.pr_url }}{% if entry.is_current %} 👈{% endif %}
    ///   {%- endfor %}
    #[expect(
        clippy::doc_lazy_continuation,
        reason = "endfor must align with the for-loop, not the list item"
    )]
    #[arg(long, env = "STAKK_TEMPLATE", verbatim_doc_comment)]
    pub template: Option<String>,

    /// Where to place the stack comment on each pull request.
    ///
    /// In body mode the stack is written inside a fenced section
    /// (STAKK_BODY_START / STAKK_BODY_END) that is appended to the PR
    /// description. Content you write outside the fences is preserved.
    /// Do not edit the fenced section by hand — it is overwritten on
    /// every run.
    ///
    /// Switching modes migrates automatically: moving to body mode
    /// deletes the old stack comment, and moving to comment mode strips
    /// the fenced section from the PR body.
    #[arg(
        long = "stack-placement",
        env = "STAKK_STACK_PLACEMENT",
        default_value = "comment",
        value_enum,
        verbatim_doc_comment
    )]
    pub stack_placement: StackPlacement,

    /// [EXPERIMENTAL] Shell command for generating custom bookmark names.
    ///
    /// This feature is experimental and may change or be removed in future
    /// releases.
    ///
    /// The command is invoked via `sh -c <command>` (Unix) or `cmd /C
    /// <command>` (Windows). It receives a JSON object on stdin describing
    /// a single segment of commits and must print exactly one bookmark name
    /// to stdout (plain text, leading/trailing whitespace is trimmed).
    ///
    /// The custom name appears as an additional [*] toggle option in the
    /// TUI, after the existing bookmarks [x] and generated name [+].
    ///
    /// JSON input schema:
    ///
    ///   rules               — object with validation constraints
    ///     .max_length       — integer, max name length in bytes (255)
    ///     .disallowed_chars — string of forbidden characters
    ///   commits             — array of commit objects, ordered
    ///                         trunk-to-tip (oldest first); the last
    ///                         element is the tip being bookmarked
    ///
    /// Each commit object:
    ///
    ///   commit_id           — full hex commit hash (string)
    ///   change_id           — full jj change ID (string)
    ///   short_change_id     — shortest unique change ID prefix (string)
    ///   description         — full commit message incl. body (string)
    ///   author              — object with name, email, timestamp
    ///     .name             — author name (string)
    ///     .email            — author email (string)
    ///     .timestamp        — commit timestamp (string, ISO 8601)
    ///   files               — array of file paths changed by this commit
    ///                         (array of strings, e.g. ["src/main.rs"])
    ///
    /// Minimal example (two commits):
    ///
    ///   {
    ///     "rules": {
    ///       "max_length": 255,
    ///       "disallowed_chars": " ~^:?*[\\"
    ///     },
    ///     "commits": [
    ///       {
    ///         "commit_id": "aaa111",
    ///         "change_id": "abc123",
    ///         "short_change_id": "abc",
    ///         "description": "add login page",
    ///         "author": {
    ///           "name": "Jo",
    ///           "email": "jo@example.com",
    ///           "timestamp": "2026-03-01T12:00:00+01:00"
    ///         },
    ///         "files": ["src/login.rs"]
    ///       },
    ///       {
    ///         "commit_id": "bbb222",
    ///         "change_id": "def456",
    ///         "short_change_id": "def",
    ///         "description": "style login form",
    ///         "author": {
    ///           "name": "Jo",
    ///           "email": "jo@example.com",
    ///           "timestamp": "2026-03-01T13:00:00+01:00"
    ///         },
    ///         "files": ["src/login.rs", "styles/login.css"]
    ///       }
    ///     ]
    ///   }
    ///
    /// Expected stdout (one line, trimmed):
    ///
    ///   login-page
    #[arg(
        long = "experimental-bookmark-command",
        env = "STAKK_EXPERIMENTAL_BOOKMARK_COMMAND",
        verbatim_doc_comment
    )]
    pub bookmark_command: Option<String>,
}
