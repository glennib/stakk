//! Data types for change graph construction.

use std::collections::HashMap;
use std::collections::HashSet;

/// A commit within a bookmark segment, carrying metadata needed for display
/// and later PR creation.
#[derive(Debug, Clone)]
pub struct SegmentCommit {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for PR creation")
    )]
    pub commit_id: String,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for PR creation")
    )]
    pub change_id: String,
    pub description: String,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for PR creation")
    )]
    pub author_name: String,
}

/// A group of consecutive commits belonging to one or more bookmarks.
///
/// When multiple bookmarks point at the same change, they share one segment.
/// Commits are ordered newest-first (the bookmarked commit is first).
#[derive(Debug, Clone)]
pub struct BookmarkSegment {
    /// Bookmark names pointing at this segment's change.
    pub bookmark_names: Vec<String>,
    /// The change ID that the bookmarks point to.
    pub change_id: String,
    /// Commits in this segment (newest first). The first commit is the one the
    /// bookmarks point at.
    pub commits: Vec<SegmentCommit>,
}

/// A complete path from trunk to a leaf bookmark.
///
/// Segments are ordered bottom-to-top: the first segment is closest to trunk,
/// the last is the leaf.
#[derive(Debug, Clone)]
pub struct BranchStack {
    pub segments: Vec<BookmarkSegment>,
}

/// The complete change graph: all bookmarked segments, their relationships,
/// and the resulting stacks.
#[derive(Debug)]
pub struct ChangeGraph {
    /// Child change_id → parent change_id (toward trunk). Each entry
    /// represents a stacking relationship between two bookmarked changes.
    pub adjacency_list: HashMap<String, String>,

    /// Change IDs that are leaf nodes (no children point to them as parent).
    /// Each leaf defines one stack.
    pub stack_leaves: HashSet<String>,

    /// Change IDs closest to trunk with no parent in the adjacency list.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for submission")
    )]
    pub stack_roots: HashSet<String>,

    /// Map from change_id to its `BookmarkSegment`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for submission")
    )]
    pub segments: HashMap<String, BookmarkSegment>,

    /// Change IDs of merge commits and their descendants, excluded from
    /// stacking.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in later milestones for diagnostics")
    )]
    pub tainted_change_ids: HashSet<String>,

    /// Number of bookmarks excluded due to merge commits in their history.
    pub excluded_bookmark_count: usize,

    /// Complete stacks, one per leaf bookmark, ordered trunk-to-leaf.
    pub stacks: Vec<BranchStack>,
}
