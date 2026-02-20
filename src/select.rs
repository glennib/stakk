//! Interactive bookmark selection using inquire.
//!
//! Two-stage selection: pick a stack, then pick a bookmark within that stack.
//! Uses `inquire::Select` for built-in pagination and type-to-filter.

use std::collections::HashMap;
use std::fmt;
use std::io;

use inquire::InquireError;

use crate::error::StakkError;
use crate::graph::types::{BranchStack, ChangeGraph};

/// Stage 1 item: a stack shown as a single line.
#[derive(Debug, Clone)]
pub struct StackChoice {
    /// Index into `ChangeGraph::stacks`.
    pub stack_index: usize,
    /// Bookmark names in trunk-to-leaf order.
    pub bookmark_names: Vec<String>,
    /// Total number of commits across all segments.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used in tests to verify commit metadata")
    )]
    pub commit_count: usize,
    /// Bookmarks shared with other stacks: (bookmark_name, [other stack leaf
    /// names]).
    pub shared_with: Vec<(String, Vec<String>)>,
    /// First commit summary of the leaf segment.
    pub leaf_summary: String,
}

impl fmt::Display for StackChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chain = self.bookmark_names.join(" \u{2190} "); // ←
        let trunk_marker = "\u{25cb} \u{2190} "; // ○ ←
        let pr_count = self.bookmark_names.len();
        if pr_count == 1 {
            write!(f, "{trunk_marker}{chain}  (1 PR: {})", self.leaf_summary)?;
        } else {
            write!(f, "{trunk_marker}{chain}  ({pr_count} PRs)")?;
        }

        for (name, other_leaves) in &self.shared_with {
            let others = other_leaves.join(", ");
            write!(f, "  [{name} also in {others}]")?;
        }

        Ok(())
    }
}

/// Stage 2 item: a bookmark within a stack.
#[derive(Debug, Clone)]
pub struct BookmarkChoice {
    /// The bookmark name.
    pub bookmark_name: String,
    /// Position in the original stack (0 = closest to trunk).
    pub segment_index: usize,
    /// Total segments in the stack.
    pub stack_len: usize,
    /// First line of each commit description in this segment.
    pub commit_summaries: Vec<String>,
}

impl fmt::Display for BookmarkChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let position = if self.stack_len <= 1 {
            ""
        } else if self.segment_index == self.stack_len - 1 {
            "leaf, "
        } else if self.segment_index == 0 {
            "base, "
        } else {
            ""
        };

        let count = self.commit_summaries.len();
        let commit_label = if count == 1 { "commit" } else { "commits" };
        let pr_count = self.segment_index + 1;
        let pr_label = if pr_count == 1 { "PR" } else { "PRs" };
        write!(
            f,
            "{} ({position}{count} {commit_label}) \u{2192} {pr_count} {pr_label}",
            self.bookmark_name,
        )?;

        for summary in &self.commit_summaries {
            write!(f, "\n    {summary}")?;
        }

        Ok(())
    }
}

/// Build stack choices from the change graph, detecting shared segments.
pub fn collect_stack_choices(graph: &ChangeGraph) -> Vec<StackChoice> {
    if graph.stacks.is_empty() {
        return Vec::new();
    }

    // Pre-pass: for each change_id, collect which stack indices contain it.
    let mut change_to_stacks: HashMap<&str, Vec<usize>> = HashMap::new();
    for (stack_idx, stack) in graph.stacks.iter().enumerate() {
        for segment in &stack.segments {
            change_to_stacks
                .entry(&segment.change_id)
                .or_default()
                .push(stack_idx);
        }
    }

    // Build leaf name lookup: stack_index -> leaf bookmark name.
    let leaf_names: Vec<String> = graph
        .stacks
        .iter()
        .map(|stack| {
            stack
                .segments
                .last()
                .and_then(|s| s.bookmark_names.first())
                .cloned()
                .unwrap_or_else(|| "(unnamed)".to_string())
        })
        .collect();

    graph
        .stacks
        .iter()
        .enumerate()
        .map(|(stack_idx, stack)| {
            let bookmark_names: Vec<String> = stack
                .segments
                .iter()
                .map(|s| {
                    s.bookmark_names
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "(unnamed)".to_string())
                })
                .collect();

            let commit_count: usize =
                stack.segments.iter().map(|s| s.commits.len()).sum();

            // Detect shared segments.
            let mut shared_with: Vec<(String, Vec<String>)> = Vec::new();
            for segment in &stack.segments {
                let name = segment
                    .bookmark_names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "(unnamed)".to_string());

                let other_leaves: Vec<String> = change_to_stacks
                    .get(segment.change_id.as_str())
                    .map(|indices| {
                        indices
                            .iter()
                            .copied()
                            .filter(|&i| i != stack_idx)
                            .map(|i| leaf_names[i].clone())
                            .collect()
                    })
                    .unwrap_or_default();

                if !other_leaves.is_empty() {
                    shared_with.push((name, other_leaves));
                }
            }

            let leaf_summary = stack
                .segments
                .last()
                .and_then(|s| s.commits.first())
                .and_then(|c| c.description.lines().next())
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .unwrap_or("(no description)")
                .to_string();

            StackChoice {
                stack_index: stack_idx,
                bookmark_names,
                commit_count,
                shared_with,
                leaf_summary,
            }
        })
        .collect()
}

/// Build bookmark choices from a stack, in leaf-first order.
pub fn collect_bookmark_choices(stack: &BranchStack) -> Vec<BookmarkChoice> {
    let stack_len = stack.segments.len();

    stack
        .segments
        .iter()
        .enumerate()
        .rev() // leaf-first
        .map(|(segment_index, segment)| {
            let bookmark_name = segment
                .bookmark_names
                .first()
                .cloned()
                .unwrap_or_else(|| "(unnamed)".to_string());

            let commit_summaries: Vec<String> = segment
                .commits
                .iter()
                .map(|c| {
                    c.description
                        .lines()
                        .next()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty())
                        .unwrap_or("(no description)")
                        .to_string()
                })
                .collect();

            BookmarkChoice {
                bookmark_name,
                segment_index,
                stack_len,
                commit_summaries,
            }
        })
        .collect()
}

/// Map inquire errors to StakkError.
fn map_inquire_error(err: InquireError) -> StakkError {
    match err {
        InquireError::NotTTY => StakkError::NotInteractive,
        InquireError::OperationCanceled
        | InquireError::OperationInterrupted => StakkError::PromptCancelled,
        InquireError::IO(e) => StakkError::Io(e),
        other => StakkError::Io(io::Error::other(other.to_string())),
    }
}

/// Show inquire stack selector, returning the chosen stack index.
fn select_stack(choices: Vec<StackChoice>) -> Result<usize, StakkError> {
    let result = inquire::Select::new("Which stack?", choices)
        .prompt()
        .map_err(map_inquire_error)?;
    Ok(result.stack_index)
}

/// Show inquire bookmark selector, returning the chosen bookmark name.
fn select_bookmark_in_stack(
    choices: Vec<BookmarkChoice>,
) -> Result<String, StakkError> {
    let result = inquire::Select::new(
        "Submit up to which bookmark?",
        choices,
    )
    .with_help_message(
        "All bookmarks from base up to your selection will be submitted",
    )
    .prompt()
    .map_err(map_inquire_error)?;
    Ok(result.bookmark_name)
}

/// Resolve a bookmark interactively from the change graph.
///
/// - No stacks: prints a message, returns `Ok(None)`.
/// - Single bookmark across all stacks: auto-selects it, returns
///   `Ok(Some(name))`.
/// - Multiple stacks: stage 1 (pick stack) then stage 2 (pick bookmark).
/// - Single stack with multiple bookmarks: skip stage 1, show stage 2 only.
///
/// Returns `StakkError::NotInteractive` if stdin is not a terminal.
/// Returns `StakkError::PromptCancelled` if the user presses Escape.
pub fn resolve_bookmark_interactively(
    graph: &ChangeGraph,
) -> Result<Option<String>, StakkError> {
    if graph.stacks.is_empty() {
        eprintln!("No bookmark stacks found.");
        return Ok(None);
    }

    // Count total bookmarks across all stacks.
    let total_bookmarks: usize =
        graph.stacks.iter().map(|s| s.segments.len()).sum();

    if total_bookmarks == 1 {
        let segment = &graph.stacks[0].segments[0];
        let name = segment
            .bookmark_names
            .first()
            .cloned()
            .unwrap_or_else(|| "(unnamed)".to_string());
        let summary = segment
            .commits
            .first()
            .and_then(|c| c.description.lines().next())
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .unwrap_or("(no description)");
        eprintln!("Auto-selecting the only bookmark: {name} ({summary})");
        return Ok(Some(name));
    }

    // Determine which stack to use.
    let stack_index = if graph.stacks.len() == 1 {
        // Skip stage 1.
        0
    } else {
        let choices = collect_stack_choices(graph);
        select_stack(choices)?
    };

    let stack = &graph.stacks[stack_index];

    // Determine which bookmark within the stack.
    if stack.segments.len() == 1 {
        // Skip stage 2 -- auto-select the only bookmark in this stack.
        let name = stack.segments[0]
            .bookmark_names
            .first()
            .cloned()
            .unwrap_or_else(|| "(unnamed)".to_string());
        return Ok(Some(name));
    }

    let choices = collect_bookmark_choices(stack);
    let name = select_bookmark_in_stack(choices)?;

    Ok(Some(name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{
        BookmarkSegment, BranchStack, ChangeGraph, SegmentCommit,
    };
    use std::collections::{HashMap, HashSet};

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: HashSet::new(),
            stack_roots: HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: HashSet::new(),
            excluded_bookmark_count: 0,
            stacks,
        }
    }

    fn make_segment(
        names: &[&str],
        change_id: &str,
        descriptions: &[&str],
    ) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(|s| s.to_string()).collect(),
            change_id: change_id.to_string(),
            commits: descriptions
                .iter()
                .enumerate()
                .map(|(i, desc)| SegmentCommit {
                    commit_id: format!("c_{change_id}_{i}"),
                    change_id: change_id.to_string(),
                    description: desc.to_string(),
                    author_name: "Test".to_string(),
                })
                .collect(),
        }
    }

    // -- StackChoice tests --

    #[test]
    fn stack_choices_empty_graph() {
        let graph = make_graph(vec![]);
        let choices = collect_stack_choices(&graph);
        assert!(choices.is_empty());
    }

    #[test]
    fn stack_choices_single_stack() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["add base"]),
                make_segment(&["leaf"], "ch_b", &["add leaf"]),
            ],
        }]);

        let choices = collect_stack_choices(&graph);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].stack_index, 0);
        assert_eq!(choices[0].bookmark_names, vec!["base", "leaf"]);
        assert_eq!(choices[0].commit_count, 2);
        assert!(choices[0].shared_with.is_empty());
    }

    #[test]
    fn stack_choices_multiple_stacks() {
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["alpha"], "ch_alpha", &["alpha"]),
                    make_segment(&["beta"], "ch_beta", &["beta"]),
                ],
            },
            BranchStack {
                segments: vec![make_segment(
                    &["gamma"],
                    "ch_gamma",
                    &["gamma"],
                )],
            },
        ]);

        let choices = collect_stack_choices(&graph);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].bookmark_names, vec!["alpha", "beta"]);
        assert_eq!(choices[1].bookmark_names, vec!["gamma"]);
        assert!(choices[0].shared_with.is_empty());
        assert!(choices[1].shared_with.is_empty());
    }

    #[test]
    fn stack_choices_shared_ancestor() {
        // Two stacks sharing a root segment (same change_id).
        let graph = make_graph(vec![
            BranchStack {
                segments: vec![
                    make_segment(&["base"], "ch_shared", &["shared base"]),
                    make_segment(&["feat-b"], "ch_b", &["feature b"]),
                ],
            },
            BranchStack {
                segments: vec![
                    make_segment(
                        &["base"],
                        "ch_shared",
                        &["shared base"],
                    ),
                    make_segment(
                        &["other-leaf"],
                        "ch_c",
                        &["other leaf"],
                    ),
                ],
            },
        ]);

        let choices = collect_stack_choices(&graph);
        assert_eq!(choices.len(), 2);

        // Stack 0: base shared with stack 1 (leaf = "other-leaf").
        assert_eq!(choices[0].shared_with.len(), 1);
        assert_eq!(choices[0].shared_with[0].0, "base");
        assert_eq!(choices[0].shared_with[0].1, vec!["other-leaf"]);

        // Stack 1: base shared with stack 0 (leaf = "feat-b").
        assert_eq!(choices[1].shared_with.len(), 1);
        assert_eq!(choices[1].shared_with[0].0, "base");
        assert_eq!(choices[1].shared_with[0].1, vec!["feat-b"]);
    }

    #[test]
    fn stack_choices_display_format() {
        let choice = StackChoice {
            stack_index: 0,
            bookmark_names: vec![
                "base".to_string(),
                "feat-b".to_string(),
                "feat-c".to_string(),
            ],
            commit_count: 5,
            shared_with: vec![(
                "base".to_string(),
                vec!["other-leaf".to_string()],
            )],
            leaf_summary: "add caching".to_string(),
        };

        let display = format!("{choice}");
        assert_eq!(
            display,
            "\u{25cb} \u{2190} base \u{2190} feat-b \u{2190} feat-c  (3 PRs)  [base also in other-leaf]"
        );
    }

    #[test]
    fn stack_choices_display_no_sharing() {
        let choice = StackChoice {
            stack_index: 0,
            bookmark_names: vec!["standalone".to_string()],
            commit_count: 1,
            shared_with: vec![],
            leaf_summary: "fix login bug".to_string(),
        };

        let display = format!("{choice}");
        assert_eq!(display, "\u{25cb} \u{2190} standalone  (1 PR: fix login bug)");
    }

    // -- BookmarkChoice tests --

    #[test]
    fn bookmark_choices_single_segment() {
        let stack = BranchStack {
            segments: vec![make_segment(
                &["only-one"],
                "ch_a",
                &["the commit"],
            )],
        };

        let choices = collect_bookmark_choices(&stack);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].bookmark_name, "only-one");
        assert_eq!(choices[0].segment_index, 0);
        assert_eq!(choices[0].stack_len, 1);
        assert_eq!(choices[0].commit_summaries, vec!["the commit"]);
    }

    #[test]
    fn bookmark_choices_multi_segment() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["base commit"]),
                make_segment(&["middle"], "ch_b", &["middle commit"]),
                make_segment(&["leaf"], "ch_c", &["leaf commit"]),
            ],
        };

        let choices = collect_bookmark_choices(&stack);
        assert_eq!(choices.len(), 3);

        // Leaf-first order.
        assert_eq!(choices[0].bookmark_name, "leaf");
        assert_eq!(choices[0].segment_index, 2);
        assert_eq!(choices[1].bookmark_name, "middle");
        assert_eq!(choices[1].segment_index, 1);
        assert_eq!(choices[2].bookmark_name, "base");
        assert_eq!(choices[2].segment_index, 0);
    }

    #[test]
    fn bookmark_choices_position_labels() {
        let stack = BranchStack {
            segments: vec![
                make_segment(&["base"], "ch_a", &["base work"]),
                make_segment(&["mid"], "ch_b", &["mid work"]),
                make_segment(&["leaf"], "ch_c", &["leaf work"]),
            ],
        };

        let choices = collect_bookmark_choices(&stack);

        let leaf_display = format!("{}", choices[0]);
        assert!(
            leaf_display.starts_with("leaf (leaf, 1 commit) \u{2192} 3 PRs"),
            "expected leaf position label and 3 PRs in '{leaf_display}'"
        );

        let mid_display = format!("{}", choices[1]);
        assert!(
            mid_display.starts_with("mid (1 commit) \u{2192} 2 PRs"),
            "expected no position label and 2 PRs in '{mid_display}'"
        );

        let base_display = format!("{}", choices[2]);
        assert!(
            base_display.starts_with("base (base, 1 commit) \u{2192} 1 PR"),
            "expected base position label and 1 PR in '{base_display}'"
        );
    }

    #[test]
    fn bookmark_choices_empty_description() {
        let stack = BranchStack {
            segments: vec![make_segment(&["feat"], "ch_a", &[""])],
        };

        let choices = collect_bookmark_choices(&stack);
        assert_eq!(
            choices[0].commit_summaries,
            vec!["(no description)"]
        );

        let display = format!("{}", choices[0]);
        assert!(display.contains("(no description)"));
    }

    #[test]
    fn bookmark_choices_display_shows_commits() {
        let stack = BranchStack {
            segments: vec![
                make_segment(
                    &["base"],
                    "ch_a",
                    &["add user model"],
                ),
                make_segment(
                    &["feat"],
                    "ch_b",
                    &[
                        "refactor auth module",
                        "extract token parser",
                    ],
                ),
            ],
        };

        let choices = collect_bookmark_choices(&stack);
        // leaf-first: feat, then base
        let feat_display = format!("{}", choices[0]);
        assert_eq!(
            feat_display,
            "feat (leaf, 2 commits) \u{2192} 2 PRs\n    refactor auth module\n    extract token parser"
        );

        let base_display = format!("{}", choices[1]);
        assert_eq!(
            base_display,
            "base (base, 1 commit) \u{2192} 1 PR\n    add user model"
        );
    }

    // -- resolve_bookmark_interactively edge cases --

    #[test]
    fn resolve_no_stacks() {
        let graph = make_graph(vec![]);
        let result = resolve_bookmark_interactively(&graph).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_single_bookmark_auto_select() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![make_segment(
                &["only-bm"],
                "ch_a",
                &["the commit"],
            )],
        }]);

        let result = resolve_bookmark_interactively(&graph).unwrap();
        assert_eq!(result, Some("only-bm".to_string()));
    }
}
