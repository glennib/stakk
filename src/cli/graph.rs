use clap::Args;

/// Arguments controlling graph discovery revsets.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Revset passed to `jj bookmark list -r <REVSET>` to discover
    /// bookmarks for graph construction.
    ///
    /// The result determines which bookmarks appear as named segments
    /// in the stack graph. Each returned bookmark is then traversed
    /// toward trunk to build the full commit chain.
    #[arg(
        long,
        default_value = "mine() ~ trunk() ~ immutable()",
        env = "STAKK_BOOKMARKS_REVSET",
        verbatim_doc_comment
    )]
    pub bookmarks_revset: String,

    /// Revset passed to `jj log -r <REVSET>` to discover unbookmarked
    /// head changes for graph construction.
    ///
    /// Each returned change becomes a traversal starting point, walked
    /// toward trunk to discover segments that have no bookmark yet.
    /// The revset should typically return only leaf commits (use
    /// `heads(...)`) to avoid redundant traversals.
    #[arg(
        long,
        default_value = "heads((mine() ~ empty() ~ immutable()) & trunk()..)",
        env = "STAKK_HEADS_REVSET",
        verbatim_doc_comment
    )]
    pub heads_revset: String,
}
