use clap::Args;

/// Arguments controlling graph discovery revsets.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Revset passed to `jj bookmark list -r <REVSET>` to discover
    /// bookmarks.
    ///
    /// The matched bookmarks become the named segments of the stack
    /// graph; each is traversed toward trunk to build its commit chain.
    #[arg(
        long,
        default_value = "mine() ~ trunk() ~ immutable()",
        env = "STAKK_BOOKMARKS_REVSET",
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
        verbatim_doc_comment
    )]
    pub heads_revset: String,
}
