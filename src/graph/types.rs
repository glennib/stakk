//! Data types for change graph construction.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::jj::types::Signature;

/// A commit within a bookmark segment, carrying metadata needed for display
/// and later PR creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCommit {
    pub commit_id: String,
    pub change_id: String,
    pub description: String,
    pub author: Signature,
    pub committer: Signature,
    /// Shortest unique change ID prefix (from jj).
    pub short_change_id: String,
    /// Files changed by this commit.
    pub files: Vec<String>,
    /// Whether jj considers the commit immutable.
    pub is_immutable: bool,
    /// All local bookmark names on this commit, unfiltered — unlike
    /// `BookmarkSegment::bookmark_names`, this includes bookmarks excluded
    /// from the graph by the bookmarks revset (e.g. on immutable commits).
    pub local_bookmark_names: Vec<String>,
    /// Remote bookmarks pointing at this commit, as jj spells them
    /// (`name@remote`), including the internal `name@git` entries.
    ///
    /// A remote bookmark sits wherever the remote currently is, which is not
    /// where the local bookmark is once the local one moves.
    /// [`ChangeGraph::bookmark_remote_states`] combines the two.
    pub remote_bookmark_names: Vec<String>,
}

/// A group of consecutive commits belonging to one or more bookmarks.
///
/// When multiple bookmarks point at the same change, they share one segment.
/// Commits are ordered newest-first (the bookmarked commit is first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkSegment {
    /// Bookmark names pointing at this segment's change.
    pub bookmark_names: Vec<String>,
    /// The change ID that the bookmarks point to.
    pub change_id: String,
    /// Commits in this segment (newest first). The first commit is the one the
    /// bookmarks point at.
    pub commits: Vec<SegmentCommit>,
}

/// Where a local bookmark stands relative to its remote counterpart.
///
/// Derived offline, from `jj` alone: it says nothing about pull requests,
/// only about what a push would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    /// No remote counterpart. Pushing creates the remote bookmark.
    Unpushed,
    /// A remote counterpart exists but sits elsewhere — the usual state
    /// after a rebase or an amend. Pushing moves it.
    Diverged,
    /// The remote counterpart is on the same commit. A push is a no-op.
    Synced,
}

impl RemoteState {
    /// The wire name used in `stakk graph`'s JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unpushed => "unpushed",
            Self::Diverged => "diverged",
            Self::Synced => "synced",
        }
    }
}

/// A complete path from trunk to a leaf bookmark.
///
/// Segments are ordered bottom-to-top: the first segment is closest to trunk,
/// the last is the leaf.
#[derive(Debug, Clone)]
pub struct BranchStack {
    pub segments: Vec<BookmarkSegment>,
}

impl BranchStack {
    /// All commits of the stack in trunk-to-tip order (oldest first).
    ///
    /// Segments are stored trunk-to-leaf but each segment's commits are
    /// newest-first, so commits are reversed per segment.
    pub fn commits_trunk_to_tip(&self) -> impl Iterator<Item = &SegmentCommit> {
        self.segments
            .iter()
            .flat_map(|seg| seg.commits.iter().rev())
    }

    /// The newest commit of the stack (the leaf segment's boundary commit).
    pub fn tip_commit(&self) -> Option<&SegmentCommit> {
        self.segments.last().and_then(|seg| seg.commits.first())
    }
}

/// The complete change graph: all bookmarked segments, their relationships,
/// and the resulting stacks.
#[derive(Debug)]
pub struct ChangeGraph {
    /// Child `change_id` → parent `change_id` (toward trunk). Each entry
    /// represents a stacking relationship between two bookmarked changes.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the graph-construction tests read it to pin which bookmarked change stacks \
                      on which"
        )
    )]
    pub adjacency_list: HashMap<String, String>,

    /// Change IDs that are leaf nodes (no children point to them as parent).
    /// Each leaf defines one stack.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the graph-construction tests read it to pin which changes end a stack"
        )
    )]
    pub stack_leaves: HashSet<String>,

    /// Map from `change_id` to its `BookmarkSegment`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the graph-construction tests read it to pin how commits are grouped into \
                      per-change segments"
        )
    )]
    pub segments: HashMap<String, BookmarkSegment>,

    /// Change IDs of merge commits and their descendants, excluded from
    /// stacking.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the merge-commit tests read it to pin that taint propagates from a merge to \
                      its descendants"
        )
    )]
    pub tainted_change_ids: HashSet<String>,

    /// Push state per user bookmark name, for every bookmark that reached a
    /// segment. Bookmark names are unique across the repo, so one map covers
    /// every stack the name appears in.
    pub bookmark_remote_states: HashMap<String, RemoteState>,

    /// Names of bookmarks excluded due to merge commits in their history.
    pub excluded_bookmarks: Vec<String>,

    /// Unbookmarked heads excluded for the same reason. Counted separately
    /// because they have no name to report.
    pub excluded_head_count: usize,

    /// Complete stacks, one per leaf bookmark, ordered trunk-to-leaf.
    pub stacks: Vec<BranchStack>,
}
