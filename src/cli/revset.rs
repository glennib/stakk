use clap::Args;

/// Revsets controlling which bookmarks and heads enter the change graph.
///
/// Flattened into every subcommand that builds a change graph, so the same
/// flags, environment variables and config keys mean the same thing wherever
/// they appear.
///
/// The "Revsets" help heading is set per arg, not via
/// `#[command(next_help_heading)]` on this struct: a struct-level heading
/// bleeds past the `#[command(flatten)]` point onto any unheaded args the
/// flattening command declares after it.
#[derive(Debug, Args)]
pub struct RevsetArgs {
    /// Revset passed to `jj bookmark list -r <REVSET>` to discover
    /// bookmarks.
    ///
    /// The matched bookmarks become the named segments of the stack
    /// graph; each is traversed toward trunk to build its commit chain.
    #[arg(
        long,
        default_value = "mine() ~ trunk() ~ immutable()",
        env = "STAKK_BOOKMARKS_REVSET",
        help_heading = "Revsets",
        verbatim_doc_comment
    )]
    pub bookmarks_revset: String,

    /// Revset passed to `jj log -r <REVSET>` to discover unbookmarked
    /// head changes.
    ///
    /// Each match is a traversal starting point, walked toward trunk to
    /// discover segments that have no bookmark yet. It should typically
    /// return only leaf commits (use `heads(...)`) to avoid redundant
    /// traversals.
    #[arg(
        long,
        default_value = "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        env = "STAKK_HEADS_REVSET",
        help_heading = "Revsets",
        verbatim_doc_comment
    )]
    pub heads_revset: String,
}
