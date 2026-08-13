//! Rendering for the `stakk show` subcommand.
//!
//! Both output formats derive from one source model ([`ShowData`]: the
//! `ChangeGraph` plus repo metadata) so they cannot drift. `pretty` renders
//! an always-expanded jj-log-style graph via [`crate::graph::layout`];
//! `json` serializes a schema-versioned DTO document for machine
//! consumption. Rendering is pure (data in → `String` out); all I/O stays
//! in `main`.

use std::fmt::Write as _;

use serde::Serialize;

use crate::graph::layout::CONNECTOR_TAIL;
use crate::graph::layout::CONNECTOR_TEE;
use crate::graph::layout::GUTTER_CELL;
use crate::graph::layout::GraphRow;
use crate::graph::layout::LayoutNode;
use crate::graph::layout::NODE_OTHER;
use crate::graph::layout::TRUNK_CHAR;
use crate::graph::layout::build_layout;
use crate::graph::types::BookmarkSegment;
use crate::graph::types::ChangeGraph;
use crate::jj::remote::parse_github_url;
use crate::jj::types::GitRemote;

/// Version of the JSON document emitted by `--format=json`. Bumped on
/// breaking schema changes.
const SCHEMA_VERSION: u32 = 1;

/// Everything `stakk show` renders, gathered by the caller.
pub struct ShowData<'a> {
    pub default_branch: &'a str,
    pub remotes: &'a [GitRemote],
    pub graph: &'a ChangeGraph,
    /// Extra host to treat as GitHub, besides github.com.
    pub github_host: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// JSON DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ShowReport<'a> {
    schema_version: u32,
    default_branch: &'a str,
    remotes: Vec<RemoteReport<'a>>,
    /// Bookmarks excluded from the graph due to merge commits in their
    /// history.
    excluded_bookmark_count: usize,
    /// One stack per leaf, trunk-to-leaf. Shared ancestor segments are
    /// repeated in every stack that contains them.
    stacks: Vec<StackReport<'a>>,
}

#[derive(Serialize)]
struct RemoteReport<'a> {
    name: &'a str,
    url: &'a str,
    /// `owner/repo` when the URL is a GitHub remote.
    github: Option<String>,
}

#[derive(Serialize)]
struct StackReport<'a> {
    segments: Vec<SegmentReport<'a>>,
}

#[derive(Serialize)]
struct SegmentReport<'a> {
    bookmark_names: &'a [String],
    /// Commits oldest-first (trunk side first), matching the
    /// `--bookmark-command` payload convention.
    commits: Vec<CommitReport<'a>>,
}

#[derive(Serialize)]
struct CommitReport<'a> {
    change_id: &'a str,
    short_change_id: &'a str,
    commit_id: &'a str,
    description: &'a str,
    author: AuthorReport<'a>,
    files: &'a [String],
    is_immutable: bool,
    /// All local bookmarks on this commit, including ones the bookmarks
    /// revset excluded from the graph.
    local_bookmark_names: &'a [String],
    /// Whether this commit is its segment's boundary (the commit the
    /// segment's bookmarks point at).
    is_boundary: bool,
    /// Whether this commit is the tip of its stack.
    is_leaf: bool,
}

#[derive(Serialize)]
struct AuthorReport<'a> {
    name: &'a str,
    email: &'a str,
    timestamp: &'a str,
}

/// Render the machine-readable JSON document (trailing newline included).
pub fn render_json(data: &ShowData) -> String {
    let report = build_report(data);
    let mut out = serde_json::to_string_pretty(&report).expect("ShowReport is always serializable");
    out.push('\n');
    out
}

fn build_report<'a>(data: &ShowData<'a>) -> ShowReport<'a> {
    ShowReport {
        schema_version: SCHEMA_VERSION,
        default_branch: data.default_branch,
        remotes: data
            .remotes
            .iter()
            .map(|r| RemoteReport {
                name: &r.name,
                url: &r.url,
                github: parse_github_url(&r.url, data.github_host).map(|g| g.to_string()),
            })
            .collect(),
        excluded_bookmark_count: data.graph.excluded_bookmark_count,
        stacks: data
            .graph
            .stacks
            .iter()
            .map(|stack| StackReport {
                segments: stack
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(seg_idx, segment)| {
                        segment_report(segment, seg_idx == stack.segments.len() - 1)
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn segment_report(segment: &BookmarkSegment, is_last_segment: bool) -> SegmentReport<'_> {
    let commit_count = segment.commits.len();
    SegmentReport {
        bookmark_names: &segment.bookmark_names,
        // Internal order is newest-first; the document is oldest-first.
        commits: segment
            .commits
            .iter()
            .rev()
            .enumerate()
            .map(|(idx, commit)| {
                let is_boundary = idx == commit_count - 1;
                CommitReport {
                    change_id: &commit.change_id,
                    short_change_id: &commit.short_change_id,
                    commit_id: &commit.commit_id,
                    description: &commit.description,
                    author: AuthorReport {
                        name: &commit.author.name,
                        email: &commit.author.email,
                        timestamp: &commit.author.timestamp,
                    },
                    files: &commit.files,
                    is_immutable: commit.is_immutable,
                    local_bookmark_names: &commit.local_bookmark_names,
                    is_boundary,
                    is_leaf: is_boundary && is_last_segment,
                }
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Pretty rendering
// ---------------------------------------------------------------------------

/// Render the human-readable graph view.
///
/// `colors` enables ANSI styling; pass false when stdout is not a terminal.
pub fn render_pretty(data: &ShowData, colors: bool) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "Default branch: {}", data.default_branch);
    for remote in data.remotes {
        let github = parse_github_url(&remote.url, data.github_host)
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let _ = writeln!(out, "Remote: {} {}{}", remote.name, remote.url, github);
    }
    out.push('\n');

    if data.graph.stacks.is_empty() {
        out.push_str("No bookmark stacks found.\n");
        return out;
    }

    let layout = build_layout(data.graph);
    for row in &layout.rows {
        out.push_str(&render_row(row, &layout.nodes, colors));
        out.push('\n');
    }

    if data.graph.excluded_bookmark_count > 0 {
        out.push('\n');
        let _ = writeln!(
            out,
            "({} bookmark(s) excluded due to merge commits)",
            data.graph.excluded_bookmark_count,
        );
    }

    out
}

fn render_row(row: &GraphRow, nodes: &[LayoutNode], colors: bool) -> String {
    let mut line = String::from(" ");
    match *row {
        GraphRow::Commit { node, col } => {
            for _ in 0..col {
                line.push_str(GUTTER_CELL);
            }
            let node = &nodes[node];
            if node.is_trunk {
                line.push_str(TRUNK_CHAR);
                line.push_str("  trunk");
            } else {
                line.push_str(NODE_OTHER);
                line.push_str("  ");
                line.push_str(&paint(
                    &node.short_change_id,
                    &console::Style::new().magenta(),
                    colors,
                ));
                line.push_str("  ");
                if !node.bookmark_names.is_empty() {
                    line.push_str(&paint(
                        &node.bookmark_names.join(", "),
                        &console::Style::new().green().bold(),
                        colors,
                    ));
                    line.push_str("  ");
                }
                if node.summary == "(no description)" {
                    line.push_str(&paint(
                        "(no description set)",
                        &console::Style::new().dim(),
                        colors,
                    ));
                } else {
                    let _ = write!(line, "\"{}\"", node.summary);
                }
                if let Some(hint) = node_hint(node) {
                    line.push_str("  ");
                    line.push_str(&paint(&hint, &console::Style::new().dim(), colors));
                }
            }
        }
        GraphRow::Connector { col } => {
            for _ in 0..col.saturating_sub(1) {
                line.push_str(GUTTER_CELL);
            }
            line.push_str(CONNECTOR_TEE);
            line.push_str(CONNECTOR_TAIL);
        }
    }
    line
}

/// Immutability / excluded-bookmark hint for a commit row, using the same
/// wording as the TUI's locked bookmark rows.
fn node_hint(node: &LayoutNode) -> Option<String> {
    match (node.is_immutable, node.excluded_bookmarks.is_empty()) {
        (true, false) => Some(format!(
            "(immutable — bookmark {} excluded by --bookmarks-revset)",
            node.excluded_bookmarks.join(", "),
        )),
        (true, true) => Some("(immutable)".to_string()),
        (false, false) => Some(format!(
            "(bookmark {} excluded by --bookmarks-revset)",
            node.excluded_bookmarks.join(", "),
        )),
        (false, true) => None,
    }
}

fn paint(text: &str, style: &console::Style, colors: bool) -> String {
    if colors {
        style.clone().force_styling(true).apply_to(text).to_string()
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use super::*;
    use crate::graph::types::BranchStack;
    use crate::graph::types::SegmentCommit;
    use crate::jj::types::Signature;

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: HashSet::new(),
            excluded_bookmark_count: 0,
            stacks,
        }
    }

    /// Build a segment. `descriptions` are newest-first, like the internal
    /// commit order; the first entry is the bookmarked commit.
    fn make_segment_at(
        names: &[&str],
        change_id: &str,
        descriptions: &[&str],
        timestamp: &str,
    ) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(ToString::to_string).collect(),
            change_id: change_id.to_string(),
            commits: descriptions
                .iter()
                .enumerate()
                .map(|(i, desc)| SegmentCommit {
                    commit_id: format!("c_{change_id}_{i}"),
                    change_id: change_id.to_string(),
                    description: desc.to_string(),
                    author: Signature {
                        name: "Test".to_string(),
                        email: "test@test.com".to_string(),
                        timestamp: "2026-01-01T00:00:00Z".to_string(),
                    },
                    committer: Signature {
                        name: "Test".to_string(),
                        email: "test@test.com".to_string(),
                        timestamp: timestamp.to_string(),
                    },
                    files: vec![format!("src/{change_id}_{i}.rs")],
                    short_change_id: change_id[..4.min(change_id.len())].to_string(),
                    is_immutable: false,
                    // Only the bookmarked (newest) commit carries local
                    // bookmarks, like a real graph.
                    local_bookmark_names: if i == 0 {
                        names.iter().map(ToString::to_string).collect()
                    } else {
                        vec![]
                    },
                })
                .collect(),
        }
    }

    /// Two stacks sharing a two-commit base segment, plus a commit with no
    /// description and an immutable commit carrying an excluded bookmark.
    fn sample_graph() -> ChangeGraph {
        let base = make_segment_at(
            &["base"],
            "qzvsmyxk",
            &["extend base", "add base"],
            "2026-01-01T00:00:00Z",
        );

        // The newest commit of feat-a has no description.
        let feat_a = make_segment_at(
            &["feat-a"],
            "wmtkoylq",
            &["", "feat a work"],
            "2026-03-01T00:00:00Z",
        );

        let mut feat_b = make_segment_at(
            &["feat-b"],
            "rlkvnnup",
            &["feat b work"],
            "2026-02-01T00:00:00Z",
        );
        // feat-b's commit is immutable and carries a filtered-out bookmark.
        feat_b.commits[0].is_immutable = true;
        feat_b.commits[0]
            .local_bookmark_names
            .push("old-mark".to_string());

        let mut graph = make_graph(vec![
            BranchStack {
                segments: vec![base.clone(), feat_a],
            },
            BranchStack {
                segments: vec![base, feat_b],
            },
        ]);
        graph.excluded_bookmark_count = 1;
        graph
    }

    fn sample_remotes() -> Vec<GitRemote> {
        vec![
            GitRemote {
                name: "origin".to_string(),
                url: "git@github.com:glennib/stakk.git".to_string(),
            },
            GitRemote {
                name: "mirror".to_string(),
                url: "https://gitlab.com/x/y.git".to_string(),
            },
        ]
    }

    #[test]
    fn pretty_graph_snapshot() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = ShowData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        insta::assert_snapshot!(render_pretty(&data, false));
    }

    #[test]
    fn pretty_no_stacks() {
        let graph = make_graph(vec![]);
        let remotes = sample_remotes();
        let data = ShowData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let out = render_pretty(&data, false);
        assert!(out.contains("No bookmark stacks found."));
        assert!(out.starts_with("Default branch: main\n"));
    }

    #[test]
    fn json_snapshot() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = ShowData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        insta::assert_snapshot!(render_json(&data));
    }

    #[test]
    fn json_shape() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = ShowData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let v: serde_json::Value = serde_json::from_str(&render_json(&data)).unwrap();

        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["default_branch"], "main");

        assert_eq!(v["remotes"][0]["name"], "origin");
        assert_eq!(v["remotes"][0]["github"], "glennib/stakk");
        assert!(v["remotes"][1]["github"].is_null());

        assert_eq!(v["excluded_bookmark_count"], 1);

        let stacks = v["stacks"].as_array().unwrap();
        assert_eq!(stacks.len(), 2);

        // Commits are oldest-first: the base segment's trunk-side commit
        // comes first, the bookmarked commit last.
        let base_commits = stacks[0]["segments"][0]["commits"].as_array().unwrap();
        assert_eq!(base_commits[0]["description"], "add base");
        assert_eq!(base_commits[0]["is_boundary"], false);
        assert_eq!(base_commits[1]["description"], "extend base");
        assert_eq!(base_commits[1]["is_boundary"], true);
        // A boundary mid-stack is not a leaf.
        assert_eq!(base_commits[1]["is_leaf"], false);

        // The tip of each stack is a leaf.
        let feat_a_commits = stacks[0]["segments"][1]["commits"].as_array().unwrap();
        assert_eq!(feat_a_commits[1]["is_leaf"], true);

        // Full identifiers and metadata are present.
        assert_eq!(base_commits[1]["change_id"], "qzvsmyxk");
        assert_eq!(base_commits[1]["short_change_id"], "qzvs");
        assert_eq!(base_commits[1]["commit_id"], "c_qzvsmyxk_0");
        assert_eq!(base_commits[1]["author"]["email"], "test@test.com");
        assert_eq!(base_commits[1]["files"][0], "src/qzvsmyxk_0.rs");
        // Non-boundary commits carry no local bookmarks in this fixture.
        assert_eq!(
            base_commits[0]["local_bookmark_names"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        // Unfiltered local bookmarks include revset-excluded ones.
        let feat_b_commit = &stacks[1]["segments"][1]["commits"][0];
        assert_eq!(feat_b_commit["is_immutable"], true);
        assert_eq!(
            feat_b_commit["local_bookmark_names"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
