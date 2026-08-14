//! Rendering for the `stakk graph` subcommand.
//!
//! Both output formats derive from one source model ([`GraphData`]: the
//! `ChangeGraph` plus repo metadata) so they cannot drift. `pretty` renders
//! an always-expanded jj-log-style graph via [`crate::graph::layout`];
//! `json` serializes a schema-versioned DTO document for machine
//! consumption. Rendering is pure (data in → `String` out); all I/O stays
//! in `main`.
//!
//! The JSON document has two projections, [`JsonProjection::Sparse`] and
//! [`JsonProjection::Full`], sharing one set of DTOs. Full-only fields are
//! `Option`s skipped when absent, so sparse is a strict subset of full by
//! construction: same names, same types, same values, same order.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::cli::GraphFormat;
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
use crate::graph::types::RemoteState;
use crate::jj::remote::parse_github_url;
use crate::jj::types::GitRemote;

/// Version of the JSON document emitted by `--format=json` and
/// `--format=json-full`. Bumped on breaking schema changes; both
/// projections always report the same version, because they are one schema.
const SCHEMA_VERSION: u32 = 2;

/// Everything `stakk graph` renders, gathered by the caller.
pub struct GraphData<'a> {
    pub default_branch: &'a str,
    pub remotes: &'a [GitRemote],
    pub graph: &'a ChangeGraph,
    /// Extra host to treat as GitHub, besides github.com.
    pub github_host: Option<&'a str>,
}

/// Which projection of the JSON document to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonProjection {
    /// Enough to pinpoint a segment and feed `stakk submit`: drops
    /// `commit_id`, `description`, `author` and `files` from every commit.
    Sparse,
    /// Every field.
    Full,
}

/// Render `data` in the format the caller asked for (trailing newline
/// included). `colors` enables ANSI styling and applies to the
/// human-readable format only; pass false when stdout is not a terminal.
///
/// This is the module's only entry point: the format decision belongs with
/// the renderer, so `main` prints what it is handed and cannot pair a
/// `--format` value with the wrong projection.
pub fn render(data: &GraphData, format: GraphFormat, colors: bool) -> String {
    match json_projection(format) {
        None => render_pretty(data, colors),
        Some(projection) => render_json(data, projection),
    }
}

/// The JSON projection a `--format` value selects, or `None` for the
/// human-readable format, which is not the JSON document at all.
fn json_projection(format: GraphFormat) -> Option<JsonProjection> {
    match format {
        GraphFormat::Pretty => None,
        GraphFormat::Json => Some(JsonProjection::Sparse),
        GraphFormat::JsonFull => Some(JsonProjection::Full),
    }
}

// ---------------------------------------------------------------------------
// JSON DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct GraphReport<'a> {
    schema_version: u32,
    default_branch: &'a str,
    remotes: Vec<RemoteReport<'a>>,
    /// Names of bookmarks excluded from the graph because of merge commits
    /// in their history.
    excluded_bookmarks: &'a [String],
    /// Unbookmarked heads excluded for the same reason. Separate from
    /// `excluded_bookmarks` because they have no name to report, so a
    /// consumer can say which bookmarks it lost and how many nameless heads.
    excluded_head_count: usize,
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
    /// The bookmarks on this segment's boundary commit, each with the push
    /// state a `stakk submit` would act on. Empty for the unbookmarked head.
    bookmarks: Vec<SegmentBookmarkReport<'a>>,
    /// Commits oldest-first (trunk side first), matching the
    /// `--bookmark-command` payload convention.
    commits: Vec<CommitReport<'a>>,
}

#[derive(Serialize)]
struct SegmentBookmarkReport<'a> {
    name: &'a str,
    /// `unpushed`, `diverged` or `synced`. Derived from jj alone, so it
    /// describes what a push would do, never whether a PR exists.
    remote_state: &'static str,
}

/// One commit. Fields typed `Option` are full-only: they are omitted, not
/// nulled, in the sparse projection. Sparse-only fields do not exist — every
/// field here is in full.
#[derive(Serialize)]
struct CommitReport<'a> {
    change_id: &'a str,
    short_change_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_id: Option<&'a str>,
    /// First line of the commit message.
    title: &'a str,
    /// The full commit message, title line included.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<AuthorReport<'a>>,
    /// The committer timestamp, in both projections because it is the key
    /// stack order is derived from: a consumer that wants a different order,
    /// or wants to verify this one, needs it without paying for `json-full`.
    committer_timestamp: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<&'a [String]>,
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
fn render_json(data: &GraphData, projection: JsonProjection) -> String {
    let report = build_report(data, projection);
    let mut out =
        serde_json::to_string_pretty(&report).expect("GraphReport is always serializable");
    out.push('\n');
    out
}

fn build_report<'a>(data: &GraphData<'a>, projection: JsonProjection) -> GraphReport<'a> {
    GraphReport {
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
        excluded_bookmarks: &data.graph.excluded_bookmarks,
        excluded_head_count: data.graph.excluded_head_count,
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
                        segment_report(
                            segment,
                            seg_idx == stack.segments.len() - 1,
                            &data.graph.bookmark_remote_states,
                            projection,
                        )
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn segment_report<'a>(
    segment: &'a BookmarkSegment,
    is_last_segment: bool,
    remote_states: &HashMap<String, RemoteState>,
    projection: JsonProjection,
) -> SegmentReport<'a> {
    let full = projection == JsonProjection::Full;
    let commit_count = segment.commits.len();
    SegmentReport {
        bookmarks: segment
            .bookmark_names
            .iter()
            .map(|name| SegmentBookmarkReport {
                name,
                // A bookmark always reaches the state map via this same
                // segment list, so the fallback is unreachable; it stays
                // rather than panicking on a graph built by hand in a test.
                remote_state: remote_states
                    .get(name)
                    .copied()
                    .unwrap_or(RemoteState::Unpushed)
                    .as_str(),
            })
            .collect(),
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
                    commit_id: full.then_some(commit.commit_id.as_str()),
                    // Empty for a commit with no description; the
                    // "(no description set)" wording is the pretty
                    // renderer's, never the document's.
                    title: commit.description.lines().next().unwrap_or(""),
                    description: full.then_some(commit.description.as_str()),
                    author: full.then(|| AuthorReport {
                        name: &commit.author.name,
                        email: &commit.author.email,
                        timestamp: &commit.author.timestamp,
                    }),
                    committer_timestamp: &commit.committer.timestamp,
                    files: full.then_some(commit.files.as_slice()),
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
fn render_pretty(data: &GraphData, colors: bool) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "Default branch: {}", data.default_branch);
    for remote in data.remotes {
        let github = parse_github_url(&remote.url, data.github_host)
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let _ = writeln!(out, "Remote: {} {}{}", remote.name, remote.url, github);
    }
    out.push('\n');

    // The exclusion footers below run in both cases. When every bookmark's
    // history contains a merge, all of them are excluded and no stack
    // survives — the one state where the footer is the whole answer.
    if data.graph.stacks.is_empty() {
        out.push_str("No bookmark stacks found.\n");
    } else {
        let layout = build_layout(data.graph);
        for row in &layout.rows {
            out.push_str(&render_row(row, &layout.nodes, colors));
            out.push('\n');
        }
    }

    if !data.graph.excluded_bookmarks.is_empty() {
        out.push('\n');
        let _ = writeln!(
            out,
            "({} excluded due to merge commits)",
            data.graph.excluded_bookmarks.join(", "),
        );
    }
    if data.graph.excluded_head_count > 0 {
        if data.graph.excluded_bookmarks.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "({} unbookmarked head(s) excluded due to merge commits)",
            data.graph.excluded_head_count,
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
            bookmark_remote_states: HashMap::new(),
            excluded_bookmarks: Vec::new(),
            excluded_head_count: 0,
            stacks,
        }
    }

    /// Build a segment. `commits` are `(change_id, description)` pairs,
    /// newest-first like the internal commit order; the first entry is the
    /// bookmarked commit, and its change id is the segment's. Every commit
    /// carries its own change id, as in a real jj graph, and the ids differ
    /// within their first four characters so `short_change_id` stays unique
    /// too.
    fn make_segment_at(
        names: &[&str],
        commits: &[(&str, &str)],
        timestamp: &str,
    ) -> BookmarkSegment {
        BookmarkSegment {
            bookmark_names: names.iter().map(ToString::to_string).collect(),
            change_id: commits[0].0.to_string(),
            commits: commits
                .iter()
                .enumerate()
                .map(|(i, (change_id, desc))| SegmentCommit {
                    commit_id: format!("c_{change_id}"),
                    change_id: (*change_id).to_string(),
                    description: (*desc).to_string(),
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
                    files: vec![format!("src/{change_id}.rs")],
                    short_change_id: change_id[..4.min(change_id.len())].to_string(),
                    is_immutable: false,
                    // Only the bookmarked (newest) commit carries local
                    // bookmarks, like a real graph.
                    local_bookmark_names: if i == 0 {
                        names.iter().map(ToString::to_string).collect()
                    } else {
                        vec![]
                    },
                    remote_bookmark_names: vec![],
                })
                .collect(),
        }
    }

    /// A description with a title line, a blank line and a body, so `title`
    /// and `description` are distinguishable in the fixture.
    const MULTILINE_DESCRIPTION: &str =
        "feat b work\n\nThe body explains why.\nIt spans two lines.";

    /// Two stacks sharing a two-commit base segment, plus a commit with no
    /// description, a commit with a multi-line description, and an immutable
    /// commit carrying an excluded bookmark. One bookmark of each push state,
    /// so both projections render all three.
    fn sample_graph() -> ChangeGraph {
        let base = make_segment_at(
            &["base"],
            &[("qzvsmyxk", "extend base"), ("mnrqxtvo", "add base")],
            "2026-01-01T00:00:00Z",
        );

        // The newest commit of feat-a has no description.
        let feat_a = make_segment_at(
            &["feat-a"],
            &[("wmtkoylq", ""), ("ptszrkwu", "feat a work")],
            "2026-03-01T00:00:00Z",
        );

        let mut feat_b = make_segment_at(
            &["feat-b"],
            &[("rlkvnnup", MULTILINE_DESCRIPTION)],
            "2026-02-01T00:00:00Z",
        );
        // feat-b's commit is immutable and carries a filtered-out bookmark.
        feat_b.commits[0].is_immutable = true;
        feat_b.commits[0]
            .local_bookmark_names
            .push("old-mark".to_string());

        let mut base = base;
        base.commits[0]
            .remote_bookmark_names
            .push("base@origin".to_string());

        let mut graph = make_graph(vec![
            BranchStack {
                segments: vec![base.clone(), feat_a],
            },
            BranchStack {
                segments: vec![base, feat_b],
            },
        ]);
        graph.bookmark_remote_states = HashMap::from([
            ("base".to_string(), RemoteState::Synced),
            ("feat-a".to_string(), RemoteState::Unpushed),
            ("feat-b".to_string(), RemoteState::Diverged),
        ]);
        graph.excluded_bookmarks = vec!["merged-work".to_string()];
        graph.excluded_head_count = 1;
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
        let data = GraphData {
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
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let out = render_pretty(&data, false);
        assert!(out.contains("No bookmark stacks found."));
        assert!(out.starts_with("Default branch: main\n"));
    }

    /// Excluding every bookmark leaves no stack, and that is exactly when the
    /// exclusion footers carry the whole answer: without them `stakk graph`
    /// reports "no stacks" and never says the merge commits are why.
    #[test]
    fn pretty_reports_exclusions_when_no_stack_survives() {
        let mut graph = make_graph(vec![]);
        graph.excluded_bookmarks = vec!["bm_merge".to_string()];
        graph.excluded_head_count = 1;
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let out = render_pretty(&data, false);
        assert!(out.contains("No bookmark stacks found."));
        assert!(out.contains("(bm_merge excluded due to merge commits)"));
        assert!(out.contains("(1 unbookmarked head(s) excluded due to merge commits)"));
    }

    fn sample_json(projection: JsonProjection) -> serde_json::Value {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        serde_json::from_str(&render_json(&data, projection)).unwrap()
    }

    /// The same document, reached the way the binary reaches it: through a
    /// `GraphFormat` value, with the projection chosen inside [`render`].
    /// `sample_json` takes the projection as an argument and so pins the
    /// document's shape but nothing about which `--format` produces it.
    fn sample_json_via_format(format: GraphFormat) -> serde_json::Value {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        serde_json::from_str(&render(&data, format, false)).unwrap()
    }

    #[test]
    fn json_snapshot() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        insta::assert_snapshot!(render_json(&data, JsonProjection::Sparse));
    }

    #[test]
    fn json_full_snapshot() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        insta::assert_snapshot!(render_json(&data, JsonProjection::Full));
    }

    /// Every value reachable in `sparse` must be reachable at the same path
    /// in `full`, with an identical value. This is the C9 invariant:
    /// one schema, two projections.
    fn assert_subset(sparse: &serde_json::Value, full: &serde_json::Value, path: &str) {
        match (sparse, full) {
            (serde_json::Value::Object(s), serde_json::Value::Object(f)) => {
                for (key, value) in s {
                    let sub = f
                        .get(key)
                        .unwrap_or_else(|| panic!("{path}.{key} missing from the full document"));
                    assert_subset(value, sub, &format!("{path}.{key}"));
                }
            }
            (serde_json::Value::Array(s), serde_json::Value::Array(f)) => {
                assert_eq!(s.len(), f.len(), "{path} length differs");
                for (i, (value, sub)) in s.iter().zip(f).enumerate() {
                    assert_subset(value, sub, &format!("{path}[{i}]"));
                }
            }
            (s, f) => assert_eq!(s, f, "{path} differs"),
        }
    }

    #[test]
    fn sparse_is_a_strict_subset_of_full() {
        let sparse = sample_json(JsonProjection::Sparse);
        let full = sample_json(JsonProjection::Full);
        assert_ne!(sparse, full, "the two projections must differ");
        assert_subset(&sparse, &full, "$");
    }

    /// Number of commits `sample_graph` renders (base 2 + feat-a 2, then
    /// base 2 again + feat-b 1). Every "check all commits" loop asserts
    /// against it, so a loop that iterates nothing cannot pass.
    const SAMPLE_COMMIT_COUNT: usize = 7;

    /// Every commit object in the document, in emitted order.
    fn all_commits(v: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
        v["stacks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|stack| stack["segments"].as_array().unwrap())
            .flat_map(|segment| segment["commits"].as_array().unwrap())
            .map(|commit| commit.as_object().unwrap())
            .collect()
    }

    #[test]
    fn sparse_omits_full_only_fields() {
        let v = sample_json(JsonProjection::Sparse);
        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["default_branch"], "main");
        assert_eq!(v["remotes"][0]["github"], "glennib/stakk");
        assert_eq!(v["excluded_bookmarks"][0], "merged-work");
        assert_eq!(v["excluded_head_count"], 1);

        // Checked on every commit, not just the first: a full-only field
        // gated on `is_leaf` or immutability would otherwise slip through.
        let commits = all_commits(&v);
        assert_eq!(commits.len(), SAMPLE_COMMIT_COUNT, "no commits inspected");
        for (i, commit) in commits.iter().enumerate() {
            // Omitted, not nulled — a `null` would break jq consumers
            // differently from an absent key.
            for field in ["commit_id", "description", "author", "files"] {
                assert!(
                    commit.get(field).is_none(),
                    "{field} leaked into sparse commit {i}"
                );
            }
            // serde_json's Map sorts keys, so this pins the field *set*
            // only; `sparse_field_order_matches_full` pins the order.
            assert_eq!(
                commit.keys().collect::<Vec<_>>(),
                vec![
                    "change_id",
                    "committer_timestamp",
                    "is_boundary",
                    "is_immutable",
                    "is_leaf",
                    "local_bookmark_names",
                    "short_change_id",
                    "title",
                ],
                "commit {i}",
            );
        }
    }

    /// Emitted field order of a sparse commit object.
    const SPARSE_COMMIT_FIELDS: [&str; 8] = [
        "\"change_id\"",
        "\"short_change_id\"",
        "\"title\"",
        "\"committer_timestamp\"",
        "\"is_immutable\"",
        "\"local_bookmark_names\"",
        "\"is_boundary\"",
        "\"is_leaf\"",
    ];

    /// Emitted field order of a full commit object.
    const FULL_COMMIT_FIELDS: [&str; 12] = [
        "\"change_id\"",
        "\"short_change_id\"",
        "\"commit_id\"",
        "\"title\"",
        "\"description\"",
        "\"author\"",
        "\"committer_timestamp\"",
        "\"files\"",
        "\"is_immutable\"",
        "\"local_bookmark_names\"",
        "\"is_boundary\"",
        "\"is_leaf\"",
    ];

    /// The rendered text of each commit object, one slice per commit.
    ///
    /// Every commit object starts with `"change_id"`, and the leading quote
    /// keeps the marker from matching inside `"short_change_id"`, so the
    /// marker splits the document exactly at commit boundaries. Callers
    /// assert the slice count, which catches a stray marker inside a string
    /// value.
    fn commit_chunks(rendered: &str) -> Vec<&str> {
        let starts: Vec<usize> = rendered
            .match_indices("\"change_id\"")
            .map(|(at, _)| at)
            .collect();
        starts
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                let end = starts.get(i + 1).copied().unwrap_or(rendered.len());
                &rendered[start..end]
            })
            .collect()
    }

    /// Assert that every commit object in `rendered` emits `fields` in that
    /// order. Scanned per commit object, never across the whole document:
    /// a document-wide cursor scan accepts a permutation inside one commit
    /// by matching the later keys in the commits that follow.
    fn assert_commit_field_order(rendered: &str, fields: &[&str], label: &str) {
        let chunks = commit_chunks(rendered);
        assert_eq!(
            chunks.len(),
            SAMPLE_COMMIT_COUNT,
            "{label}: unexpected commit object count"
        );
        for (i, chunk) in chunks.iter().enumerate() {
            let mut cursor = 0;
            for field in fields {
                let at = chunk[cursor..].find(field).unwrap_or_else(|| {
                    panic!("{label} commit {i}: {field} is missing or out of order in:\n{chunk}")
                });
                cursor += at + field.len();
            }
        }
    }

    /// Sparse must be an ordered subsequence of full, not just a subset of
    /// its keys: a consumer diffing the two documents should see removals
    /// only. Asserted on the rendered text, since a parsed
    /// `serde_json::Value` sorts its keys and loses the emitted order.
    #[test]
    fn sparse_field_order_matches_full() {
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let sparse = render_json(&data, JsonProjection::Sparse);
        let full = render_json(&data, JsonProjection::Full);

        assert_commit_field_order(&sparse, &SPARSE_COMMIT_FIELDS, "sparse");
        assert_commit_field_order(&full, &FULL_COMMIT_FIELDS, "full");

        // The full-only fields sit between sparse fields, so dropping them
        // leaves the surviving order untouched: the sparse order is a
        // subsequence of the full one.
        let mut full_fields = FULL_COMMIT_FIELDS.iter();
        for field in SPARSE_COMMIT_FIELDS {
            assert!(
                full_fields.any(|f| *f == field),
                "{field} does not appear in the full field order after the fields before it"
            );
        }
    }

    #[test]
    fn title_is_the_first_line_of_the_description() {
        let full = sample_json(JsonProjection::Full);
        let base = &full["stacks"][0]["segments"][0]["commits"][0];
        assert_eq!(base["title"], "add base");
        assert_eq!(base["description"], "add base");

        // A multi-line message: the title is the first line alone, the
        // description the whole message with that line still in it.
        let feat_b = &full["stacks"][1]["segments"][1]["commits"][0];
        assert!(
            feat_b.is_object(),
            "the multi-line fixture commit is not where the test looks"
        );
        assert_eq!(feat_b["title"], "feat b work");
        assert_eq!(feat_b["description"], MULTILINE_DESCRIPTION);
        assert_ne!(
            feat_b["title"], feat_b["description"],
            "title must not be the whole description"
        );
        assert_eq!(
            sample_json(JsonProjection::Sparse)["stacks"][1]["segments"][1]["commits"][0]["title"],
            "feat b work"
        );

        // A commit with no description has an empty title — never the
        // pretty renderer's "(no description set)" wording.
        let feat_a_tip = &full["stacks"][0]["segments"][1]["commits"][1];
        assert_eq!(feat_a_tip["title"], "");
        assert_eq!(feat_a_tip["description"], "");
        assert_eq!(
            sample_json(JsonProjection::Sparse)["stacks"][0]["segments"][1]["commits"][1]["title"],
            ""
        );
    }

    /// Which commit fields each `--format` value actually emits, asserted
    /// through [`render`] — the entry point the binary calls — rather than
    /// through a projection the test picks itself. A test that names a
    /// `JsonProjection` cannot tell a correct mapping from one that emits
    /// the full document for every format.
    ///
    /// Key sets are sorted here because `serde_json::Map` sorts them;
    /// `sparse_field_order_matches_full` pins the emitted order.
    #[test]
    fn render_emits_the_projection_the_format_selects() {
        let sparse_keys = [
            "change_id",
            "committer_timestamp",
            "is_boundary",
            "is_immutable",
            "is_leaf",
            "local_bookmark_names",
            "short_change_id",
            "title",
        ];
        let full_keys = [
            "author",
            "change_id",
            "commit_id",
            "committer_timestamp",
            "description",
            "files",
            "is_boundary",
            "is_immutable",
            "is_leaf",
            "local_bookmark_names",
            "short_change_id",
            "title",
        ];

        for (format, expected) in [
            (GraphFormat::Json, sparse_keys.as_slice()),
            (GraphFormat::JsonFull, full_keys.as_slice()),
        ] {
            let v = sample_json_via_format(format);
            let commits = all_commits(&v);
            assert_eq!(
                commits.len(),
                SAMPLE_COMMIT_COUNT,
                "{format:?}: no commits inspected"
            );
            for (i, commit) in commits.iter().enumerate() {
                assert_eq!(
                    commit.keys().collect::<Vec<_>>(),
                    expected.iter().collect::<Vec<_>>(),
                    "{format:?} commit {i}",
                );
            }
        }

        // Pretty is not the JSON document at all.
        let graph = sample_graph();
        let remotes = sample_remotes();
        let data = GraphData {
            default_branch: "main",
            remotes: &remotes,
            graph: &graph,
            github_host: None,
        };
        let pretty = render(&data, GraphFormat::Pretty, false);
        assert!(
            pretty.starts_with("Default branch: main\n"),
            "pretty rendered as: {pretty}"
        );
    }

    /// Pins the flag → projection wiring `main` routes through: swapping the
    /// two JSON arms would make `--format=json` emit the full document.
    #[test]
    fn json_projection_per_format() {
        assert_eq!(json_projection(GraphFormat::Pretty), None);
        assert_eq!(
            json_projection(GraphFormat::Json),
            Some(JsonProjection::Sparse)
        );
        assert_eq!(
            json_projection(GraphFormat::JsonFull),
            Some(JsonProjection::Full)
        );
    }

    #[test]
    fn json_shape() {
        let v = sample_json(JsonProjection::Full);

        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["default_branch"], "main");

        assert_eq!(v["remotes"][0]["name"], "origin");
        assert_eq!(v["remotes"][0]["github"], "glennib/stakk");
        assert!(v["remotes"][1]["github"].is_null());

        assert_eq!(v["excluded_bookmarks"][0], "merged-work");
        assert_eq!(v["excluded_head_count"], 1);

        let stacks = v["stacks"].as_array().unwrap();
        assert_eq!(stacks.len(), 2);

        // Each segment names its bookmarks with the push state a submission
        // would act on. All three states appear in the fixture.
        assert_eq!(stacks[0]["segments"][0]["bookmarks"][0]["name"], "base");
        assert_eq!(
            stacks[0]["segments"][0]["bookmarks"][0]["remote_state"],
            "synced"
        );
        assert_eq!(
            stacks[0]["segments"][1]["bookmarks"][0]["remote_state"],
            "unpushed"
        );
        assert_eq!(
            stacks[1]["segments"][1]["bookmarks"][0]["remote_state"],
            "diverged"
        );

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

        // `committer_timestamp` is the committer's, not the author's: the
        // fixture gives every commit the same author timestamp and a
        // per-segment committer timestamp, and stack order follows the
        // latter.
        assert_eq!(
            base_commits[1]["author"]["timestamp"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            feat_a_commits[1]["author"]["timestamp"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            feat_a_commits[1]["committer_timestamp"],
            "2026-03-01T00:00:00Z"
        );

        // Full identifiers and metadata are present.
        assert_eq!(base_commits[1]["change_id"], "qzvsmyxk");
        assert_eq!(base_commits[1]["short_change_id"], "qzvs");
        assert_eq!(base_commits[1]["commit_id"], "c_qzvsmyxk");
        assert_eq!(base_commits[1]["author"]["email"], "test@test.com");
        assert_eq!(base_commits[1]["files"][0], "src/qzvsmyxk.rs");
        // Every commit in a segment has its own change id, so the
        // `--new REV` identifier a consumer reads out of the document is
        // never ambiguous.
        assert_eq!(base_commits[0]["change_id"], "mnrqxtvo");
        assert_eq!(base_commits[0]["short_change_id"], "mnrq");
        // A shared segment repeats identically in every stack that carries
        // it, as `stakk graph` re-emits shared ancestors per stack.
        let base_again = stacks[1]["segments"][0]["commits"].as_array().unwrap();
        assert_eq!(base_again, base_commits);
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
