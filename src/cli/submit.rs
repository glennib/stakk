use clap::Args;
use clap::ValueEnum;

use crate::cli::revset::RevsetArgs;
use crate::forge::comment::StackPlacement;

/// Whether new pull requests are created as regular or draft PRs.
///
/// This only affects newly created PRs. Existing PRs keep their
/// current draft/ready state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PrMode {
    /// Create pull requests as regular (non-draft) PRs.
    #[default]
    Regular,
    /// Create pull requests as drafts.
    Draft,
}

impl std::fmt::Display for PrMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pv = self
            .to_possible_value()
            .expect("all variants have possible values");
        f.write_str(pv.get_name())
    }
}

/// Controls whether existing PR titles and/or bodies are updated from
/// commit descriptions on every submit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SyncPrContent {
    /// Do not sync. Title and body are only set on PR creation.
    #[default]
    None,
    /// Sync only the PR title from the first line of the commit description.
    Title,
    /// Sync only the PR body from the commit description.
    Body,
    /// Sync both the PR title and body.
    All,
}

impl std::fmt::Display for SyncPrContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pv = self
            .to_possible_value()
            .expect("all variants have possible values");
        f.write_str(pv.get_name())
    }
}

/// Controls whether git commit trailers are stripped from PR bodies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum TrailerHandling {
    /// Leave trailers in the PR body verbatim.
    #[default]
    Keep,
    /// Strip the trailer block (Signed-off-by, Co-authored-by, Refs, etc.)
    /// from the PR body.
    Strip,
}

impl std::fmt::Display for TrailerHandling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pv = self
            .to_possible_value()
            .expect("all variants have possible values");
        f.write_str(pv.get_name())
    }
}

/// Arguments for the submit subcommand.
#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// Print the submission plan and stop.
    ///
    /// No bookmark is created, nothing is pushed, and no pull request is
    /// touched — but a configured --bookmark-command still runs during
    /// selection.
    #[arg(long, verbatim_doc_comment)]
    pub dry_run: bool,

    /// Keep an existing bookmark as a PR boundary (repeatable).
    ///
    /// Non-interactive selection: the --keep/--new/--new-auto/--new-command
    /// marks fully determine the PR set — nothing is implicit. All marks
    /// must lie on one trunk-to-tip path; the topmost is the tip of the
    /// submission.
    ///
    /// Unmarked commits *below* the topmost mark fold into the PR above
    /// them, bookmarked or not. Commits *above* it are not submitted at
    /// all — an unbookmarked work-in-progress head is left out unless it
    /// is marked.
    ///
    /// `stakk graph [--format=json]` lists stacks, bookmarks and change
    /// ids.
    #[arg(long, value_name = "BOOKMARK", verbatim_doc_comment)]
    pub keep: Vec<String>,

    /// Create a new bookmark at REV as a PR boundary (repeatable).
    ///
    /// REV is a change id or commit id prefix (as shown by `stakk graph`).
    /// The bookmark is named stakk-<change_id> unless =NAME is given.
    #[arg(long, value_name = "REV[=NAME]", verbatim_doc_comment)]
    pub new: Vec<String>,

    /// Create a new auto-named bookmark at REV (repeatable).
    ///
    /// The name is derived with TF-IDF from the descriptions and files of
    /// the commits folded into the boundary, honoring --auto-prefix. Falls
    /// back to stakk-<change_id> when nothing can be derived or the
    /// derived name is already taken.
    #[arg(long, value_name = "REV", verbatim_doc_comment)]
    pub new_auto: Vec<String>,

    /// Create a new bookmark at REV named by --bookmark-command (repeatable).
    ///
    /// Errors if no bookmark command is configured. The command receives
    /// the same JSON segment description as in the TUI.
    #[arg(long, value_name = "REV", verbatim_doc_comment)]
    pub new_command: Vec<String>,

    #[command(flatten)]
    pub revset: RevsetArgs,

    /// Whether new pull requests are created as regular or draft PRs.
    ///
    /// Existing PRs keep their current draft/ready state.
    #[arg(
        long,
        env = "STAKK_PR_MODE",
        default_value = "regular",
        value_enum,
        verbatim_doc_comment
    )]
    pub pr_mode: PrMode,

    /// Git remote to push to.
    #[arg(long, default_value = "origin", env = "STAKK_REMOTE")]
    pub remote: String,

    /// Path to a custom minijinja template for stack comments.
    ///
    /// Render context:
    ///
    ///   stack             — list of entries (see below)
    ///   stack_size        — total number of entries
    ///   default_branch    — name of the trunk branch (e.g. "main")
    ///   current_bookmark  — the bookmark being submitted
    ///   stakk_url         — URL to the stakk project
    ///
    /// Each stack entry:
    ///
    ///   bookmark_name  — bookmark name
    ///   pr_url         — full URL to the pull request
    ///   pr_number      — PR number
    ///   title          — PR title
    ///   base           — base branch name
    ///   is_draft       — whether the PR is a draft
    ///   position       — 1-based position in the stack, 1 nearest the trunk
    ///   is_current     — true for the PR being submitted
    ///   is_leaf        — true for the tip of the stack
    ///
    /// Entries come trunk-first; the built-in template pipes them through
    /// `reverse` to draw the stack leaf-first, like `stakk graph`.
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
    #[arg(long, env = "STAKK_TEMPLATE_PATH", verbatim_doc_comment)]
    pub template_path: Option<String>,

    /// Where to place the stack overview on each pull request.
    ///
    /// body mode appends a fenced section (STAKK_BODY_START /
    /// STAKK_BODY_END) to the PR description. Content outside the fences
    /// is preserved; the fenced section itself is overwritten on every
    /// run, so do not edit it by hand.
    ///
    /// none retires the feature cleanly by removing existing stack
    /// comments and body fences on submit. ignore leaves them exactly as
    /// they are — use it when something else owns that part of the PR.
    ///
    /// Switching between comment and body migrates automatically: moving to
    /// body deletes the old stack comment, moving to comment strips the
    /// fenced section from the PR body.
    ///
    /// A submission that produces a single pull request is not a stack:
    /// no stack info is written, and stale artifacts are cleaned up
    /// unless the mode is ignore.
    #[arg(
        long,
        env = "STAKK_STACK_PLACEMENT",
        default_value = "comment",
        value_enum,
        verbatim_doc_comment
    )]
    pub stack_placement: StackPlacement,

    /// Whether existing PR titles and/or bodies are updated from jj commit
    /// descriptions on every submit.
    ///
    /// Syncing overwrites manual edits to the synced fields on GitHub.
    ///
    /// See --trailers for whether commit trailers are kept or stripped.
    #[arg(
        long,
        env = "STAKK_SYNC_PR_CONTENT",
        default_value = "none",
        value_enum,
        verbatim_doc_comment
    )]
    pub sync_pr_content: SyncPrContent,

    /// Whether to keep or strip git commit trailers in PR bodies.
    ///
    /// Trailers are key/value lines at the end of a commit message such as
    /// Signed-off-by, Co-authored-by, or Refs.
    #[arg(
        long,
        env = "STAKK_TRAILERS",
        default_value = "keep",
        value_enum,
        verbatim_doc_comment
    )]
    pub trailers: TrailerHandling,

    /// Prefix for auto-generated bookmark names.
    ///
    /// Prepended to names from the [~]auto generator (TF-IDF, term
    /// frequency-inverse document frequency): --auto-prefix gb- turns
    /// "caching-database" into "gb-caching-database". Does not apply to
    /// the default stakk-<change_id> names or to --bookmark-command names.
    ///
    /// The prefix is applied before length/character validation, so it
    /// counts toward the 255-byte limit.
    #[arg(long, env = "STAKK_AUTO_PREFIX", verbatim_doc_comment)]
    pub auto_prefix: Option<String>,

    /// Shell command for generating custom bookmark names.
    ///
    /// Invoked via sh -c <command> (Unix) or cmd /C <command> (Windows).
    /// It receives a JSON object on stdin describing a single segment of
    /// commits and must print exactly one bookmark name to stdout (plain
    /// text, leading/trailing whitespace is trimmed).
    ///
    /// The custom name appears as an additional [*] toggle option in the
    /// TUI, after the existing bookmarks [x] and generated name [+].
    ///
    /// JSON input schema:
    ///
    ///   schema_version      -- integer, currently 1; bumped on
    ///                          breaking schema changes
    ///   rules               -- object with validation constraints
    ///     .max_length       -- integer, max name length in bytes (255)
    ///     .disallowed_chars -- string of forbidden characters
    ///   commits             -- array of commit objects, ordered
    ///                          trunk-to-tip (oldest first); the last
    ///                          element is the tip being bookmarked
    ///
    /// Each commit object:
    ///
    ///   commit_id           -- full hex commit hash (string)
    ///   change_id           -- full jj change ID (string)
    ///   short_change_id     -- shortest unique change ID prefix (string)
    ///   description         -- full commit message incl. body (string)
    ///   author              -- object with .name, .email and .timestamp
    ///                          (ISO 8601), all strings
    ///   files               -- array of file paths changed by this commit
    ///                          (e.g. ["src/main.rs"])
    ///
    /// Minimal example (two commits):
    ///
    ///   {
    ///     "schema_version": 1,
    ///     "rules": { "max_length": 255, "disallowed_chars": " ~^:?*[\\" },
    ///     "commits": [
    ///       {
    ///         "commit_id": "aaa111",
    ///         "change_id": "abc123",
    ///         "short_change_id": "abc",
    ///         "description": "add login page",
    ///         "author": { "name": "Jo", "email": "jo@example.com",
    ///                     "timestamp": "2026-03-01T12:00:00+01:00" },
    ///         "files": ["src/login.rs"]
    ///       },
    ///       {
    ///         "commit_id": "bbb222",
    ///         "change_id": "def456",
    ///         "short_change_id": "def",
    ///         "description": "style login form",
    ///         "author": { "name": "Jo", "email": "jo@example.com",
    ///                     "timestamp": "2026-03-01T13:00:00+01:00" },
    ///         "files": ["src/login.rs", "styles/login.css"]
    ///       }
    ///     ]
    ///   }
    ///
    /// Expected stdout (one line, trimmed): login-page
    ///
    /// Example command (lowercase the tip commit description, replace
    /// non-alphanumeric runs with hyphens, trim to 50 chars):
    ///
    ///   jq -r '.commits[-1].description' \
    ///     | tr '[:upper:]' '[:lower:]' \
    ///     | sed 's/[^a-z0-9]\{1,\}/-/g; s/^-//; s/-$//' \
    ///     | head -c 50
    #[arg(long, env = "STAKK_BOOKMARK_COMMAND", verbatim_doc_comment)]
    pub bookmark_command: Option<String>,
}
