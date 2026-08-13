//! Non-interactive bookmark selection driven by CLI flags.
//!
//! `--keep`, `--keep-all`, `--new REV[=NAME]`, `--new-auto REV`, and
//! `--new-command REV` fully determine the PR boundary set — nothing is
//! implicit. The marks themselves define the stack: all must lie on one
//! trunk-to-tip path (colinearity is validated), and the topmost mark is
//! the tip of the submission. Bookmarks on the path that are not kept fold
//! into the PR above them, exactly like unchecked rows in the TUI.
//!
//! The resolver is pure with respect to the repository: it reads the
//! already-built [`ChangeGraph`] and produces a [`SelectionResult`]; all
//! bookmark creation happens later, in the execute phase.

use std::collections::BTreeSet;
use std::collections::HashSet;

use miette::Diagnostic;
use thiserror::Error;

use super::SelectionResult;
use super::bookmark_gen;
use super::bookmark_gen::BookmarkGenError;
use super::tfidf;
use crate::cli::submit::SubmitArgs;
use crate::graph::types::ChangeGraph;
use crate::graph::types::SegmentCommit;
use crate::submit::BookmarkAssignment;

/// Errors from resolving the explicit selection flags.
#[derive(Debug, Error, Diagnostic)]
pub enum ExplicitSelectionError {
    /// A `--new` value that is neither `REV` nor `REV=NAME`.
    #[error("invalid --new value {arg:?}: expected REV or REV=NAME")]
    #[diagnostic(
        code(stakk::selection::invalid_new_spec),
        help(
            "pass a change or commit id prefix, optionally followed by =NAME; run `stakk show` to \
             list ids"
        )
    )]
    InvalidNewSpec { arg: String },

    /// A rev-taking flag received an empty value.
    #[error("{flag} requires a non-empty REV")]
    #[diagnostic(
        code(stakk::selection::empty_rev),
        help("pass a change or commit id prefix; run `stakk show` to list ids")
    )]
    EmptyRev { flag: String },

    /// No submittable stacks exist at all.
    #[error("no bookmark stacks found")]
    #[diagnostic(
        code(stakk::selection::no_stacks),
        help(
            "there is nothing to submit; check --bookmarks-revset / --heads-revset, or run `stakk \
             show` to inspect the repository"
        )
    )]
    NoStacks,

    /// A rev matched no commit on any stack.
    #[error("revision {rev:?} does not match any commit on a submittable stack")]
    #[diagnostic(
        code(stakk::selection::rev_not_found),
        help(
            "the rev must prefix-match a change or commit id on a stack; trunk, immutable, and \
             revset-excluded commits are not submittable — run `stakk show` (or `stakk show \
             --format=json`) to list candidates"
        )
    )]
    RevNotFound { rev: String },

    /// A rev matched more than one distinct commit.
    #[error("revision {rev:?} is ambiguous: matches {}", candidates.join(", "))]
    #[diagnostic(
        code(stakk::selection::rev_ambiguous),
        help("use a longer prefix; run `stakk show` to see short change ids")
    )]
    RevAmbiguous {
        rev: String,
        candidates: Vec<String>,
    },

    /// A rev resolved to an immutable commit.
    #[error("revision {rev:?} resolves to an immutable commit")]
    #[diagnostic(
        code(stakk::selection::rev_immutable),
        help(
            "immutable commits cannot become PR boundaries; a bookmark there would be invisible \
             to subsequent runs (see --bookmarks-revset)"
        )
    )]
    RevImmutable { rev: String },

    /// A `--keep` name matched no bookmark on any stack.
    #[error("bookmark {name:?} not found on any stack")]
    #[diagnostic(
        code(stakk::selection::keep_not_found),
        help(
            "run `stakk show` to list bookmarks; only bookmarks matched by --bookmarks-revset \
             appear"
        )
    )]
    KeepNotFound { name: String },

    /// The marks do not all lie on one trunk-to-tip path.
    #[error("marks do not lie on a single trunk-to-tip path: {}", marks.join(", "))]
    #[diagnostic(
        code(stakk::selection::not_colinear),
        help(
            "all marks must be ancestors or descendants of one another — run `stakk show` to \
             inspect the stacks"
        )
    )]
    MarksNotColinear { marks: Vec<String> },

    /// Bare `--keep-all` (or one anchored on a shared segment) cannot pick
    /// a stack.
    #[error("--keep-all is ambiguous between stacks: {}", candidates.join("; "))]
    #[diagnostic(
        code(stakk::selection::keep_all_ambiguous),
        help("anchor the path with --keep or --new, or run `stakk show` to inspect the stacks")
    )]
    KeepAllAmbiguous { candidates: Vec<String> },

    /// `--keep-all` alone found nothing to keep.
    #[error("--keep-all found no bookmarks on the selected path")]
    #[diagnostic(
        code(stakk::selection::no_marks),
        help("use --new to create a bookmark, or run `stakk show`")
    )]
    NoMarks,

    /// Two marks target the same commit.
    #[error("marks {a} and {b} target the same commit")]
    #[diagnostic(
        code(stakk::selection::duplicate_mark),
        help("a commit can be at most one PR boundary; drop one of the marks")
    )]
    DuplicateMarkOnCommit { a: String, b: String },

    /// The same bookmark name would be used by more than one mark.
    #[error("bookmark name {name:?} is used by more than one mark")]
    #[diagnostic(
        code(stakk::selection::duplicate_name),
        help("bookmark names must be unique; pass an explicit name with --new REV=NAME")
    )]
    DuplicateName { name: String },

    /// A `--new` name collides with an existing bookmark.
    #[error("bookmark {name:?} already exists")]
    #[diagnostic(
        code(stakk::selection::name_exists),
        help(
            "choose another name, or pass --keep with the existing name instead — `jj bookmark \
             list` shows every local bookmark, including ones outside the selectable stacks"
        )
    )]
    NewNameExists { name: String },

    /// `--new-command` was passed without a configured bookmark command.
    #[error("--new-command requires a bookmark command")]
    #[diagnostic(
        code(stakk::selection::bookmark_command_not_configured),
        help(
            "pass --bookmark-command, set STAKK_BOOKMARK_COMMAND, or add bookmark_command to \
             stakk.toml"
        )
    )]
    BookmarkCommandNotConfigured,

    /// The bookmark command failed or produced an invalid name.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Gen(#[from] BookmarkGenError),
}

/// A `--new REV[=NAME]` value, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBookmarkSpec {
    /// Change or commit id prefix.
    pub rev: String,
    /// Explicit name (`REV=NAME` form), or `None` for the
    /// `stakk-<change_id>` default.
    pub name: Option<String>,
}

/// The parsed selection flags of one `stakk submit` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionSpec {
    pub keep: Vec<String>,
    pub keep_all: bool,
    pub new: Vec<NewBookmarkSpec>,
    pub new_auto: Vec<String>,
    pub new_command: Vec<String>,
}

impl SelectionSpec {
    /// Parse the selection flags out of the CLI args.
    ///
    /// Splits `--new REV=NAME` values and validates explicit names, so
    /// name errors surface as miette diagnostics rather than clap usage
    /// errors.
    pub fn from_args(args: &SubmitArgs) -> Result<Self, ExplicitSelectionError> {
        let mut new = Vec::with_capacity(args.new.len());
        for raw in &args.new {
            let spec = if let Some((rev, name)) = raw.split_once('=') {
                if rev.is_empty() || name.is_empty() {
                    return Err(ExplicitSelectionError::InvalidNewSpec { arg: raw.clone() });
                }
                bookmark_gen::validate_bookmark_name(name)?;
                NewBookmarkSpec {
                    rev: rev.to_string(),
                    name: Some(name.to_string()),
                }
            } else {
                if raw.is_empty() {
                    return Err(ExplicitSelectionError::InvalidNewSpec { arg: raw.clone() });
                }
                NewBookmarkSpec {
                    rev: raw.clone(),
                    name: None,
                }
            };
            new.push(spec);
        }
        // An empty rev would prefix-match every commit; reject it here
        // (clap accepts "" as a value).
        for (revs, flag) in [
            (&args.new_auto, "--new-auto"),
            (&args.new_command, "--new-command"),
        ] {
            if revs.iter().any(String::is_empty) {
                return Err(ExplicitSelectionError::EmptyRev {
                    flag: flag.to_string(),
                });
            }
        }
        Ok(Self {
            keep: args.keep.clone(),
            keep_all: args.keep_all,
            new,
            new_auto: args.new_auto.clone(),
            new_command: args.new_command.clone(),
        })
    }

    /// Whether no selection flag was passed at all (→ interactive TUI).
    pub fn is_empty(&self) -> bool {
        self.keep.is_empty()
            && !self.keep_all
            && self.new.is_empty()
            && self.new_auto.is_empty()
            && self.new_command.is_empty()
    }
}

/// How a mark produces its bookmark name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkKind {
    /// Existing bookmark, kept as-is.
    Keep { name: String },
    /// New bookmark: explicit name, or the `stakk-<change_id>` default.
    New { name: Option<String> },
    /// New bookmark named by TF-IDF over the dynamic segment.
    NewAuto,
    /// New bookmark named by the external bookmark command.
    NewCommand,
}

/// A mark resolved to a concrete commit, before path selection.
#[derive(Debug)]
struct Mark {
    kind: MarkKind,
    /// The flag occurrence, for error messages (e.g. `--new abc=foo`).
    display: String,
    commit_id: String,
    /// Indices into `graph.stacks` of every stack containing the commit.
    stacks: BTreeSet<usize>,
}

/// Resolve the explicit selection flags against the change graph.
///
/// Pure with respect to the repository: reads the graph, returns the
/// assignments and the selected trunk-to-tip path. `async` only because
/// `--new-command` awaits the external command.
///
/// `reserved` is every local bookmark name in the repo
/// ([`crate::jj::Jj::get_local_bookmark_names`]). New names are checked
/// against it rather than against the graph, which cannot see trunk's own
/// bookmark or anything the bookmarks revset filtered out.
pub async fn resolve_bookmarks_explicitly(
    graph: &ChangeGraph,
    spec: &SelectionSpec,
    auto_prefix: Option<&str>,
    bookmark_command: Option<&str>,
    reserved: &HashSet<String>,
) -> Result<SelectionResult, ExplicitSelectionError> {
    if !spec.new_command.is_empty() && bookmark_command.is_none() {
        return Err(ExplicitSelectionError::BookmarkCommandNotConfigured);
    }
    if graph.stacks.is_empty() {
        return Err(ExplicitSelectionError::NoStacks);
    }

    // Linearized trunk-to-tip commit chains, one per stack.
    let linearized: Vec<Vec<&SegmentCommit>> = graph
        .stacks
        .iter()
        .map(|s| s.commits_trunk_to_tip().collect())
        .collect();

    let mut marks: Vec<Mark> = Vec::new();

    // Resolve rev marks (--new / --new-auto / --new-command).
    let rev_marks = spec
        .new
        .iter()
        .map(|n| {
            let display = match &n.name {
                Some(name) => format!("--new {}={name}", n.rev),
                None => format!("--new {}", n.rev),
            };
            (
                n.rev.clone(),
                MarkKind::New {
                    name: n.name.clone(),
                },
                display,
            )
        })
        .chain(
            spec.new_auto
                .iter()
                .map(|rev| (rev.clone(), MarkKind::NewAuto, format!("--new-auto {rev}"))),
        )
        .chain(spec.new_command.iter().map(|rev| {
            (
                rev.clone(),
                MarkKind::NewCommand,
                format!("--new-command {rev}"),
            )
        }));
    for (rev, kind, display) in rev_marks {
        let (commit, stacks) = resolve_rev(&linearized, &rev)?;
        marks.push(Mark {
            kind,
            display,
            commit_id: commit.commit_id.clone(),
            stacks,
        });
    }

    // Resolve --keep names; identical repeats dedupe silently.
    let mut kept_names: HashSet<&str> = HashSet::new();
    for name in &spec.keep {
        if !kept_names.insert(name.as_str()) {
            continue;
        }
        let (commit, stacks) = resolve_keep(&linearized, graph, name)?;
        marks.push(Mark {
            kind: MarkKind::Keep { name: name.clone() },
            display: format!("--keep {name}"),
            commit_id: commit.commit_id.clone(),
            stacks,
        });
    }

    // Colinearity: all marks must share at least one stack.
    let candidate_stacks: BTreeSet<usize> =
        match marks
            .iter()
            .map(|m| &m.stacks)
            .fold(None::<BTreeSet<usize>>, |acc, s| match acc {
                None => Some(s.clone()),
                Some(acc) => Some(acc.intersection(s).copied().collect()),
            }) {
            Some(set) => {
                if set.is_empty() {
                    return Err(ExplicitSelectionError::MarksNotColinear {
                        marks: marks.iter().map(|m| m.display.clone()).collect(),
                    });
                }
                set
            }
            // No marks yet: bare --keep-all considers every stack.
            None => (0..graph.stacks.len()).collect(),
        };

    // Expand --keep-all into Keep marks on the chosen path. Commits that
    // already carry an explicit mark are skipped (explicit beats bulk).
    if spec.keep_all {
        let anchored = !marks.is_empty();
        let expansion = keep_all_expansion(graph, &candidate_stacks, &marks)?;
        if expansion.is_empty() && !anchored {
            return Err(ExplicitSelectionError::NoMarks);
        }
        marks.extend(expansion);
    }

    // Per-commit duplicate check.
    {
        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for mark in &marks {
            if let Some(first) = seen.insert(mark.commit_id.as_str(), mark.display.as_str()) {
                return Err(ExplicitSelectionError::DuplicateMarkOnCommit {
                    a: first.to_string(),
                    b: mark.display.clone(),
                });
            }
        }
    }

    // Name checks that don't need generation: duplicates among explicit
    // names, and --new names colliding with any existing local bookmark.
    {
        let mut seen: HashSet<&str> = HashSet::new();
        for mark in &marks {
            let name = match &mark.kind {
                MarkKind::Keep { name } => Some(name.as_str()),
                MarkKind::New { name } => name.as_deref(),
                MarkKind::NewAuto | MarkKind::NewCommand => None,
            };
            let Some(name) = name else { continue };
            if !seen.insert(name) {
                return Err(ExplicitSelectionError::DuplicateName {
                    name: name.to_string(),
                });
            }
            if matches!(mark.kind, MarkKind::New { .. }) && reserved.contains(name) {
                return Err(ExplicitSelectionError::NewNameExists {
                    name: name.to_string(),
                });
            }
        }
    }

    // Pick the path. Every candidate stack contains all marks; where they
    // differ is only above the topmost mark, which the analysis drops, so
    // the first candidate is as good as any.
    let stack_idx = *candidate_stacks
        .first()
        .expect("candidate_stacks is non-empty: either checked above or built from marks");
    let path: Vec<SegmentCommit> = graph.stacks[stack_idx]
        .commits_trunk_to_tip()
        .cloned()
        .collect();

    // Sort marks trunk-to-tip by position on the path.
    let position = |commit_id: &str| {
        path.iter()
            .position(|c| c.commit_id == commit_id)
            .expect("every mark is on a candidate stack by construction")
    };
    let mut marks: Vec<(usize, Mark)> = marks
        .into_iter()
        .map(|m| (position(&m.commit_id), m))
        .collect();
    marks.sort_by_key(|(pos, _)| *pos);

    // Generate names trunk-to-tip. The dynamic segment of a mark is the
    // path slice from the previous mark (exclusive) through the mark
    // (inclusive) — the commits that fold into its PR.
    let mut assignments = Vec::with_capacity(marks.len());
    let mut used_names: HashSet<String> = HashSet::new();
    let mut segment_start = 0usize;
    for (pos, mark) in &marks {
        let commit = &path[*pos];
        let segment: Vec<&SegmentCommit> = path[segment_start..=*pos].iter().collect();
        let (name, is_new) = match &mark.kind {
            MarkKind::Keep { name } => (name.clone(), false),
            MarkKind::New { name: Some(name) } => (name.clone(), true),
            MarkKind::New { name: None } => {
                (bookmark_gen::default_bookmark_name(&commit.change_id), true)
            }
            MarkKind::NewAuto => {
                let taken = |n: &str| reserved.contains(n) || used_names.contains(n);
                (
                    auto_name(&segment, &commit.change_id, auto_prefix, taken),
                    true,
                )
            }
            MarkKind::NewCommand => {
                let command = bookmark_command
                    .expect("--new-command without a configured command errors above");
                let input = bookmark_gen::build_segment_input_from_commits(&segment);
                let json =
                    serde_json::to_string(&input).expect("SegmentInput is always serializable");
                let name =
                    bookmark_gen::run_command(command, &json, bookmark_gen::COMPUTING_TIMEOUT)
                        .await?;
                bookmark_gen::validate_bookmark_name(&name)?;
                (name, true)
            }
        };
        // Re-check duplicates now that generated names are known.
        if !used_names.insert(name.clone()) {
            return Err(ExplicitSelectionError::DuplicateName { name });
        }
        // No new name may shadow an existing bookmark. Explicit
        // `--new REV=NAME` names were already checked before generation;
        // re-checking them here is harmless and keeps the condition simple.
        if is_new && reserved.contains(name.as_str()) {
            return Err(ExplicitSelectionError::NewNameExists { name });
        }
        assignments.push(BookmarkAssignment {
            change_id: commit.change_id.clone(),
            short_change_id: commit.short_change_id.clone(),
            bookmark_name: name,
            is_new,
        });
        segment_start = *pos + 1;
    }

    Ok(SelectionResult { assignments, path })
}

/// TF-IDF name for a dynamic segment, honoring the auto prefix; falls back
/// to the `stakk-<change_id>` default when nothing can be derived or the
/// derived name is already taken (mirroring the TUI, which skips states
/// that would produce a duplicate).
fn auto_name(
    segment: &[&SegmentCommit],
    change_id: &str,
    auto_prefix: Option<&str>,
    taken: impl Fn(&str) -> bool,
) -> String {
    let data: Vec<tfidf::CommitData<'_>> = segment
        .iter()
        .map(|c| tfidf::CommitData {
            description: &c.description,
            files: &c.files,
        })
        .collect();
    match bookmark_gen::tfidf_prefixed_name(&data, 0, auto_prefix) {
        Some(name) if !taken(&name) => name,
        _ => bookmark_gen::default_bookmark_name(change_id),
    }
}

/// Resolve a rev (change or commit id prefix) to a unique mutable commit
/// and the set of stacks containing it.
fn resolve_rev<'a>(
    linearized: &[Vec<&'a SegmentCommit>],
    rev: &str,
) -> Result<(&'a SegmentCommit, BTreeSet<usize>), ExplicitSelectionError> {
    // commit_id → (commit, stack set); shared segments are cloned into
    // every containing stack, so one commit in N stacks is NOT ambiguous.
    let mut matches: Vec<(&SegmentCommit, BTreeSet<usize>)> = Vec::new();
    for (stack_idx, commits) in linearized.iter().enumerate() {
        for commit in commits {
            if commit.change_id.starts_with(rev) || commit.commit_id.starts_with(rev) {
                match matches
                    .iter_mut()
                    .find(|(c, _)| c.commit_id == commit.commit_id)
                {
                    Some((_, stacks)) => {
                        stacks.insert(stack_idx);
                    }
                    None => {
                        matches.push((commit, BTreeSet::from([stack_idx])));
                    }
                }
            }
        }
    }
    // Immutable commits cannot become boundaries, so only mutable matches
    // count toward uniqueness; immutable ones merely shape the error when
    // nothing mutable matched.
    let any_immutable = matches.iter().any(|(c, _)| c.is_immutable);
    let mut mutable: Vec<(&SegmentCommit, BTreeSet<usize>)> = matches
        .into_iter()
        .filter(|(c, _)| !c.is_immutable)
        .collect();
    match mutable.len() {
        0 if any_immutable => Err(ExplicitSelectionError::RevImmutable {
            rev: rev.to_string(),
        }),
        0 => Err(ExplicitSelectionError::RevNotFound {
            rev: rev.to_string(),
        }),
        1 => Ok(mutable.remove(0)),
        _ => Err(ExplicitSelectionError::RevAmbiguous {
            rev: rev.to_string(),
            candidates: mutable
                .iter()
                .map(|(c, _)| {
                    let summary = c.description.lines().next().unwrap_or("").trim();
                    if summary.is_empty() {
                        c.short_change_id.clone()
                    } else {
                        format!("{} {summary:?}", c.short_change_id)
                    }
                })
                .collect(),
        }),
    }
}

/// Resolve a `--keep` name to its segment's boundary commit and the set of
/// stacks containing it.
fn resolve_keep<'a>(
    linearized: &[Vec<&'a SegmentCommit>],
    graph: &ChangeGraph,
    name: &str,
) -> Result<(&'a SegmentCommit, BTreeSet<usize>), ExplicitSelectionError> {
    let mut found: Option<(&SegmentCommit, BTreeSet<usize>)> = None;
    for (stack_idx, stack) in graph.stacks.iter().enumerate() {
        for segment in &stack.segments {
            if !segment.bookmark_names.iter().any(|n| n == name) {
                continue;
            }
            let Some(boundary) = segment.commits.first() else {
                continue;
            };
            match &mut found {
                Some((commit, stacks)) if commit.commit_id == boundary.commit_id => {
                    stacks.insert(stack_idx);
                }
                Some(_) => {
                    // A bookmark points at exactly one commit; differing
                    // commit ids for one name cannot come from the graph.
                    unreachable!("bookmark {name} resolves to two different commits");
                }
                None => {
                    // Borrow the commit from the linearized view so the
                    // lifetime matches the return type.
                    let commit = linearized[stack_idx]
                        .iter()
                        .find(|c| c.commit_id == boundary.commit_id)
                        .expect("segment commits appear in the stack linearization");
                    found = Some((commit, BTreeSet::from([stack_idx])));
                }
            }
        }
    }
    found.ok_or_else(|| ExplicitSelectionError::KeepNotFound {
        name: name.to_string(),
    })
}

/// Expand `--keep-all` into Keep marks on the candidate stacks.
///
/// Commits already carrying an explicit mark are skipped (explicit beats
/// bulk). All candidate stacks must agree on the resulting expansion;
/// bare `--keep-all` therefore requires a single stack, and an anchored
/// one errors if the candidates diverge in their bookmarks.
fn keep_all_expansion(
    graph: &ChangeGraph,
    candidate_stacks: &BTreeSet<usize>,
    marks: &[Mark],
) -> Result<Vec<Mark>, ExplicitSelectionError> {
    let marked_commits: HashSet<&str> = marks.iter().map(|m| m.commit_id.as_str()).collect();

    // Per candidate stack: the (commit_id, name) list of unmarked
    // bookmarked boundaries, trunk-to-leaf.
    let mut expansions: Vec<Vec<(String, String)>> = Vec::new();
    for &stack_idx in candidate_stacks {
        let expansion: Vec<(String, String)> = graph.stacks[stack_idx]
            .segments
            .iter()
            .filter_map(|segment| {
                let name = segment.bookmark_names.first()?;
                let boundary = segment.commits.first()?;
                if marked_commits.contains(boundary.commit_id.as_str()) {
                    return None;
                }
                Some((boundary.commit_id.clone(), name.clone()))
            })
            .collect();
        expansions.push(expansion);
    }

    let first = &expansions[0];
    if expansions.iter().any(|e| e != first) {
        return Err(ExplicitSelectionError::KeepAllAmbiguous {
            candidates: candidate_stacks
                .iter()
                .map(|&idx| describe_stack(&graph.stacks[idx]))
                .collect(),
        });
    }

    Ok(first
        .iter()
        .map(|(commit_id, name)| Mark {
            kind: MarkKind::Keep { name: name.clone() },
            display: format!("--keep-all ({name})"),
            commit_id: commit_id.clone(),
            // Identical expansions mean every expanded commit is shared by
            // all candidate stacks. (Only the pre-expansion colinearity
            // fold reads this, but keep the data truthful.)
            stacks: candidate_stacks.clone(),
        })
        .collect())
}

/// One-line description of a stack for ambiguity errors: its tip's short
/// change id plus the tip segment's bookmarks or description.
fn describe_stack(stack: &crate::graph::types::BranchStack) -> String {
    let Some(tip) = stack.tip_commit() else {
        return "(empty stack)".to_string();
    };
    let names = stack
        .segments
        .last()
        .map(|s| s.bookmark_names.join(", "))
        .unwrap_or_default();
    if names.is_empty() {
        let summary = tip.description.lines().next().unwrap_or("").trim();
        format!("{} {summary:?}", tip.short_change_id)
    } else {
        format!("{} ({names})", tip.short_change_id)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::graph::types::BookmarkSegment;
    use crate::graph::types::BranchStack;
    use crate::jj::types::Signature;

    fn sig() -> Signature {
        Signature {
            name: "Test".to_string(),
            email: "test@test.com".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_commit(change_id: &str, description: &str) -> SegmentCommit {
        SegmentCommit {
            commit_id: format!("c_{change_id}"),
            change_id: change_id.to_string(),
            description: description.to_string(),
            author: sig(),
            committer: sig(),
            short_change_id: change_id[..4.min(change_id.len())].to_string(),
            files: vec![format!("src/{change_id}.rs")],
            is_immutable: false,
            local_bookmark_names: vec![],
        }
    }

    /// A one-commit segment; `names` also become the commit's local
    /// bookmarks (like a real graph).
    fn make_segment(names: &[&str], change_id: &str, description: &str) -> BookmarkSegment {
        let mut commit = make_commit(change_id, description);
        commit.local_bookmark_names = names.iter().map(ToString::to_string).collect();
        BookmarkSegment {
            bookmark_names: names.iter().map(ToString::to_string).collect(),
            change_id: change_id.to_string(),
            commits: vec![commit],
        }
    }

    fn make_graph(stacks: Vec<BranchStack>) -> ChangeGraph {
        ChangeGraph {
            adjacency_list: HashMap::new(),
            stack_leaves: std::collections::HashSet::new(),
            stack_roots: std::collections::HashSet::new(),
            segments: HashMap::new(),
            tainted_change_ids: std::collections::HashSet::new(),
            excluded_bookmark_count: 0,
            stacks,
        }
    }

    /// One stack: base <- mid <- leaf, all bookmarked.
    fn single_stack_graph() -> ChangeGraph {
        make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["base"], "aaaa1111", "base work"),
                make_segment(&["mid"], "bbbb2222", "mid work"),
                make_segment(&["leaf"], "cccc3333", "leaf work"),
            ],
        }])
    }

    /// Two stacks sharing a bookmarked base segment. The shared segment is
    /// cloned into both stacks, like `group_segments_into_stacks` does.
    fn forked_graph() -> ChangeGraph {
        let base = make_segment(&["base"], "aaaa1111", "base work");
        make_graph(vec![
            BranchStack {
                segments: vec![
                    base.clone(),
                    make_segment(&["feat-x"], "xxxx1111", "x work"),
                ],
            },
            BranchStack {
                segments: vec![base, make_segment(&["feat-y"], "yyyy1111", "y work")],
            },
        ])
    }

    fn spec(f: impl FnOnce(&mut SelectionSpec)) -> SelectionSpec {
        let mut s = SelectionSpec::default();
        f(&mut s);
        s
    }

    /// Every local bookmark name visible on the graph's stacks. Production
    /// passes a superset of this (the repo-wide list from jj); tests that do
    /// not care about the difference use it as a stand-in.
    fn reserved_from_graph(graph: &ChangeGraph) -> HashSet<String> {
        graph
            .stacks
            .iter()
            .flat_map(BranchStack::commits_trunk_to_tip)
            .flat_map(|c| c.local_bookmark_names.iter())
            .cloned()
            .collect()
    }

    async fn resolve(
        graph: &ChangeGraph,
        s: &SelectionSpec,
    ) -> Result<SelectionResult, ExplicitSelectionError> {
        let reserved = reserved_from_graph(graph);
        resolve_with_reserved(graph, s, &reserved).await
    }

    async fn resolve_with_reserved(
        graph: &ChangeGraph,
        s: &SelectionSpec,
        reserved: &HashSet<String>,
    ) -> Result<SelectionResult, ExplicitSelectionError> {
        resolve_bookmarks_explicitly(graph, s, None, None, reserved).await
    }

    fn names(result: &SelectionResult) -> Vec<(&str, bool)> {
        result
            .assignments
            .iter()
            .map(|a| (a.bookmark_name.as_str(), a.is_new))
            .collect()
    }

    // -- keep --

    #[tokio::test]
    async fn keep_subset_orders_trunk_to_leaf() {
        let graph = single_stack_graph();
        // Marks given leaf-first; assignments must come back trunk-to-leaf.
        let s = spec(|s| s.keep = vec!["leaf".into(), "base".into()]);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(names(&result), vec![("base", false), ("leaf", false)]);
        assert_eq!(result.path.len(), 3);
        assert_eq!(result.assignments.last().unwrap().bookmark_name, "leaf");
    }

    #[tokio::test]
    async fn keep_repeated_name_dedupes_silently() {
        let graph = single_stack_graph();
        let s = spec(|s| s.keep = vec!["mid".into(), "mid".into()]);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(names(&result), vec![("mid", false)]);
    }

    #[tokio::test]
    async fn keep_not_found() {
        let graph = single_stack_graph();
        let s = spec(|s| s.keep = vec!["nope".into()]);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::KeepNotFound { name } if name == "nope"));
    }

    // -- keep-all --

    #[tokio::test]
    async fn keep_all_bare_single_stack() {
        let graph = single_stack_graph();
        let s = spec(|s| s.keep_all = true);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            names(&result),
            vec![("base", false), ("mid", false), ("leaf", false)]
        );
    }

    #[tokio::test]
    async fn keep_all_bare_multi_stack_errors() {
        let graph = forked_graph();
        let s = spec(|s| s.keep_all = true);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::KeepAllAmbiguous { candidates } if candidates.len() == 2
        ));
    }

    #[tokio::test]
    async fn keep_all_anchored_picks_the_marked_stack() {
        let graph = forked_graph();
        let s = spec(|s| {
            s.keep_all = true;
            s.keep = vec!["feat-y".into()];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(names(&result), vec![("base", false), ("feat-y", false)]);
    }

    #[tokio::test]
    async fn keep_all_bare_no_bookmarks_errors_no_marks() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![{
                let mut seg = make_segment(&[], "aaaa1111", "wip");
                seg.bookmark_names = vec![];
                seg
            }],
        }]);
        let s = spec(|s| s.keep_all = true);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::NoMarks));
    }

    #[tokio::test]
    async fn keep_all_skips_explicitly_marked_commits() {
        let graph = single_stack_graph();
        // mid's commit gets an explicit --new name; keep-all must not also
        // keep `mid` there (explicit beats bulk).
        let s = spec(|s| {
            s.keep_all = true;
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: Some("renamed-mid".into()),
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            names(&result),
            vec![("base", false), ("renamed-mid", true), ("leaf", false)]
        );
    }

    // -- rev resolution --

    #[tokio::test]
    async fn rev_resolves_by_change_id_and_commit_id_prefix() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: Some("by-change".into()),
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(result.assignments[0].change_id, "bbbb2222");

        // Commit ids are `c_<change_id>` in these fixtures.
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "c_bbbb".into(),
                name: Some("by-commit".into()),
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(result.assignments[0].change_id, "bbbb2222");
    }

    #[tokio::test]
    async fn rev_not_found() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "zzzz".into(),
                name: None,
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::RevNotFound { rev } if rev == "zzzz"));
    }

    #[tokio::test]
    async fn rev_ambiguous_across_distinct_commits() {
        let graph = make_graph(vec![BranchStack {
            segments: vec![
                make_segment(&["a"], "dddd1111", "one"),
                make_segment(&["b"], "dddd2222", "two"),
            ],
        }]);
        let s = spec(|s| {
            s.new_auto = vec!["dddd".into()];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::RevAmbiguous { candidates, .. } if candidates.len() == 2
        ));
    }

    /// The shared base segment is cloned into both stacks; matching its
    /// commit must NOT count as ambiguous (dedupe by commit id).
    #[tokio::test]
    async fn rev_shared_clone_is_not_ambiguous() {
        let graph = forked_graph();
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "aaaa".into(),
                name: Some("re-base".into()),
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(names(&result), vec![("re-base", true)]);
        assert_eq!(result.assignments[0].change_id, "aaaa1111");
    }

    #[tokio::test]
    async fn rev_immutable_errors() {
        let mut graph = single_stack_graph();
        graph.stacks[0].segments[0].commits[0].is_immutable = true;
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "aaaa".into(),
                name: None,
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::RevImmutable { .. }));
    }

    // -- colinearity --

    #[tokio::test]
    async fn marks_on_diverging_stacks_error() {
        let graph = forked_graph();
        let s = spec(|s| s.keep = vec!["feat-x".into(), "feat-y".into()]);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::MarksNotColinear { marks } if marks.len() == 2
        ));
    }

    #[tokio::test]
    async fn marks_on_shared_prefix_are_colinear() {
        let graph = forked_graph();
        let s = spec(|s| s.keep = vec!["base".into(), "feat-x".into()]);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(names(&result), vec![("base", false), ("feat-x", false)]);
    }

    // -- duplicates --

    #[tokio::test]
    async fn two_marks_on_one_commit_error() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.keep = vec!["mid".into()];
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: Some("other".into()),
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::DuplicateMarkOnCommit { .. }
        ));
    }

    #[tokio::test]
    async fn duplicate_explicit_names_error() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.new = vec![
                NewBookmarkSpec {
                    rev: "aaaa".into(),
                    name: Some("same".into()),
                },
                NewBookmarkSpec {
                    rev: "cccc".into(),
                    name: Some("same".into()),
                },
            ];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::DuplicateName { name } if name == "same"
        ));
    }

    #[tokio::test]
    async fn new_name_matching_existing_bookmark_errors() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: Some("leaf".into()),
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::NewNameExists { name } if name == "leaf"
        ));
    }

    // -- name generation --

    #[tokio::test]
    async fn new_without_name_uses_stakk_default() {
        let graph = single_stack_graph();
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: None,
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            result.assignments[0].bookmark_name,
            bookmark_gen::default_bookmark_name("bbbb2222"),
        );
        assert!(result.assignments[0].is_new);
    }

    #[tokio::test]
    async fn new_auto_uses_dynamic_segment_and_prefix() {
        let mut graph = single_stack_graph();
        graph.stacks[0].segments[0].commits[0].description = "database caching layer".into();
        graph.stacks[0].segments[1].commits[0].description = "login page styling".into();
        // Mark only mid: its dynamic segment is base+mid (both folded in).
        let s = spec(|s| s.new_auto = vec!["bbbb".into()]);
        let reserved = reserved_from_graph(&graph);
        let result = resolve_bookmarks_explicitly(&graph, &s, Some("gb-"), None, &reserved)
            .await
            .unwrap();
        let name = &result.assignments[0].bookmark_name;
        assert!(name.starts_with("gb-"), "prefix applied: {name}");
        // Terms come from the folded commits, not just the boundary.
        assert!(
            name.contains("database") || name.contains("caching") || name.contains("login"),
            "tf-idf terms from the dynamic segment: {name}",
        );
    }

    #[tokio::test]
    async fn new_auto_falls_back_to_default_name() {
        let mut graph = single_stack_graph();
        // No description, no files: TF-IDF has nothing to work with.
        graph.stacks[0].segments[0].commits[0].description = String::new();
        graph.stacks[0].segments[0].commits[0].files = vec![];
        let s = spec(|s| s.new_auto = vec!["aaaa".into()]);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            result.assignments[0].bookmark_name,
            bookmark_gen::default_bookmark_name("aaaa1111"),
        );
    }

    /// Two auto marks deriving the same TF-IDF name do not collide: the
    /// second falls back to its `stakk-<change_id>` default, mirroring the
    /// TUI's skip-on-duplicate semantics.
    #[tokio::test]
    async fn second_auto_mark_with_identical_input_falls_back() {
        let mut graph = single_stack_graph();
        graph.stacks[0].segments[0].commits[0].description = "identical words here".into();
        graph.stacks[0].segments[0].commits[0].files = vec![];
        graph.stacks[0].segments[1].commits[0].description = "identical words here".into();
        graph.stacks[0].segments[1].commits[0].files = vec![];
        let s = spec(|s| s.new_auto = vec!["aaaa".into(), "bbbb".into()]);
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            result.assignments[1].bookmark_name,
            bookmark_gen::default_bookmark_name("bbbb2222"),
        );
        assert_ne!(
            result.assignments[0].bookmark_name,
            result.assignments[1].bookmark_name,
        );
    }

    /// An empty rev must be rejected at parse time — `starts_with("")`
    /// would otherwise match every commit.
    #[test]
    fn empty_rev_for_auto_and_command_flags_errors() {
        for flag in ["--new-auto", "--new-command"] {
            let args = parse_submit(&["stakk", "submit", flag, ""]);
            let err = SelectionSpec::from_args(&args).unwrap_err();
            assert!(
                matches!(err, ExplicitSelectionError::EmptyRev { .. }),
                "expected EmptyRev for {flag}",
            );
        }
    }

    /// A prefix matching one mutable and one immutable commit resolves to
    /// the mutable one — immutable commits are not submittable candidates.
    #[tokio::test]
    async fn rev_prefix_shared_with_immutable_commit_is_not_ambiguous() {
        let mut graph = single_stack_graph();
        // Both commits share the "b" prefix; make the base one immutable.
        graph.stacks[0].segments[0].commits[0].change_id = "baaa1111".into();
        graph.stacks[0].segments[0].commits[0].is_immutable = true;
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "b".into(),
                name: Some("picked".into()),
            }];
        });
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(result.assignments[0].change_id, "bbbb2222");

        // A prefix matching ONLY the immutable commit reports immutability.
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "ba".into(),
                name: Some("picked".into()),
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::RevImmutable { .. }));
    }

    /// A default-named `--new REV` whose `stakk-<change_id>` name already
    /// exists as a bookmark errors at selection time, not at execution.
    #[tokio::test]
    async fn new_default_name_colliding_with_existing_bookmark_errors() {
        let mut graph = single_stack_graph();
        let taken = bookmark_gen::default_bookmark_name("bbbb2222");
        graph.stacks[0].segments[0].bookmark_names = vec![taken.clone()];
        graph.stacks[0].segments[0].commits[0].local_bookmark_names = vec![taken];
        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: None,
            }];
        });
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::NewNameExists { .. }));
    }

    /// An auto mark whose TF-IDF name collides with an existing bookmark
    /// falls back to the default name instead of erroring.
    #[tokio::test]
    async fn auto_mark_colliding_with_existing_bookmark_falls_back() {
        let mut graph = single_stack_graph();
        graph.stacks[0].segments[1].commits[0].description = "database caching layer".into();
        graph.stacks[0].segments[1].commits[0].files = vec![];
        // Discover what TF-IDF derives for the segment, then plant an
        // existing bookmark with exactly that name on the base segment.
        let s = spec(|s| s.new_auto = vec!["bbbb".into()]);
        let derived = resolve(&graph, &s)
            .await
            .unwrap()
            .assignments
            .swap_remove(0)
            .bookmark_name;
        graph.stacks[0].segments[0].bookmark_names = vec![derived.clone()];
        graph.stacks[0].segments[0].commits[0].local_bookmark_names = vec![derived];
        let result = resolve(&graph, &s).await.unwrap();
        assert_eq!(
            result.assignments[0].bookmark_name,
            bookmark_gen::default_bookmark_name("bbbb2222"),
        );
    }

    // -- reserved names outside the graph --

    /// The collision check must use the repo-wide bookmark list, not the
    /// graph: trunk's own bookmark is dropped by the default bookmarks
    /// revset (`~ trunk()`) and trunk commits are outside the traversal
    /// range, so it is never visible on a stack.
    #[tokio::test]
    async fn new_name_colliding_with_bookmark_outside_the_graph_errors() {
        let graph = single_stack_graph();
        assert!(
            !reserved_from_graph(&graph).contains("main"),
            "precondition: trunk's bookmark is invisible to the graph",
        );
        let mut reserved = reserved_from_graph(&graph);
        reserved.insert("main".to_string());

        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: Some("main".into()),
            }];
        });
        let err = resolve_with_reserved(&graph, &s, &reserved)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::NewNameExists { name } if name == "main"
        ));
    }

    /// The same applies to the `stakk-<change_id>` default name.
    #[tokio::test]
    async fn new_default_name_colliding_outside_the_graph_errors() {
        let graph = single_stack_graph();
        let mut reserved = reserved_from_graph(&graph);
        reserved.insert(bookmark_gen::default_bookmark_name("bbbb2222"));

        let s = spec(|s| {
            s.new = vec![NewBookmarkSpec {
                rev: "bbbb".into(),
                name: None,
            }];
        });
        let err = resolve_with_reserved(&graph, &s, &reserved)
            .await
            .unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::NewNameExists { .. }));
    }

    /// An auto mark falls back to its default name when the TF-IDF name is
    /// taken by a bookmark the graph cannot see.
    #[tokio::test]
    async fn auto_mark_colliding_outside_the_graph_falls_back() {
        let mut graph = single_stack_graph();
        graph.stacks[0].segments[1].commits[0].description = "database caching layer".into();
        graph.stacks[0].segments[1].commits[0].files = vec![];
        let s = spec(|s| s.new_auto = vec!["bbbb".into()]);
        let derived = resolve(&graph, &s)
            .await
            .unwrap()
            .assignments
            .swap_remove(0)
            .bookmark_name;

        let mut reserved = reserved_from_graph(&graph);
        reserved.insert(derived);
        let result = resolve_with_reserved(&graph, &s, &reserved).await.unwrap();
        assert_eq!(
            result.assignments[0].bookmark_name,
            bookmark_gen::default_bookmark_name("bbbb2222"),
        );
    }

    /// `--keep` is unaffected: keeping a bookmark that exists is the point,
    /// and the name is resolved against the graph, not the reserved set.
    #[tokio::test]
    async fn keep_is_not_blocked_by_the_reserved_set() {
        let graph = single_stack_graph();
        let reserved = reserved_from_graph(&graph);
        assert!(reserved.contains("leaf"));
        let s = spec(|s| s.keep = vec!["leaf".into()]);
        let result = resolve_with_reserved(&graph, &s, &reserved).await.unwrap();
        assert_eq!(names(&result), vec![("leaf", false)]);
    }

    // -- bookmark command --

    #[tokio::test]
    async fn new_command_unconfigured_errors() {
        let graph = single_stack_graph();
        let s = spec(|s| s.new_command = vec!["bbbb".into()]);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::BookmarkCommandNotConfigured
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_command_runs_and_names() {
        let graph = single_stack_graph();
        let s = spec(|s| s.new_command = vec!["bbbb".into()]);
        let reserved = reserved_from_graph(&graph);
        let result =
            resolve_bookmarks_explicitly(&graph, &s, None, Some("echo from-command"), &reserved)
                .await
                .unwrap();
        assert_eq!(names(&result), vec![("from-command", true)]);
    }

    /// A name from the external command is new like any other, so it is
    /// checked against the reserved set too.
    #[cfg(unix)]
    #[tokio::test]
    async fn new_command_name_colliding_outside_the_graph_errors() {
        let graph = single_stack_graph();
        let mut reserved = reserved_from_graph(&graph);
        reserved.insert("main".to_string());
        let s = spec(|s| s.new_command = vec!["bbbb".into()]);
        let err = resolve_bookmarks_explicitly(&graph, &s, None, Some("echo main"), &reserved)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::NewNameExists { name } if name == "main"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_command_failure_propagates() {
        let graph = single_stack_graph();
        let s = spec(|s| s.new_command = vec!["bbbb".into()]);
        let reserved = reserved_from_graph(&graph);
        let err = resolve_bookmarks_explicitly(&graph, &s, None, Some("false"), &reserved)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::Gen(BookmarkGenError::CommandFailed { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_command_receives_dynamic_segment_json() {
        use std::io::Write;

        let graph = single_stack_graph();
        let tmpdir = std::env::temp_dir();
        let script = tmpdir.join("stakk_explicit_stdin.sh");
        let capture = tmpdir.join("stakk_explicit_stdin_capture.json");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "cat > {}", capture.display()).unwrap();
            writeln!(f, "echo captured-name").unwrap();
        }

        // Mark mid: the dynamic segment is base + mid, oldest first.
        let s = spec(|s| s.new_command = vec!["bbbb".into()]);
        let reserved = reserved_from_graph(&graph);
        let result = resolve_bookmarks_explicitly(
            &graph,
            &s,
            None,
            Some(&format!("sh {}", script.display())),
            &reserved,
        )
        .await
        .unwrap();
        assert_eq!(names(&result), vec![("captured-name", true)]);

        let captured = std::fs::read_to_string(&capture).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&captured).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        let commits = parsed["commits"].as_array().unwrap();
        assert_eq!(commits.len(), 2, "base folds into mid's segment");
        assert_eq!(commits[0]["change_id"], "aaaa1111");
        assert_eq!(commits[1]["change_id"], "bbbb2222");

        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(&capture);
    }

    // -- misc --

    #[tokio::test]
    async fn empty_graph_errors_no_stacks() {
        let graph = make_graph(vec![]);
        let s = spec(|s| s.keep_all = true);
        let err = resolve(&graph, &s).await.unwrap_err();
        assert!(matches!(err, ExplicitSelectionError::NoStacks));
    }

    /// End-to-end: resolve, then feed the result into
    /// `analysis_from_selection` and check fold semantics.
    #[tokio::test]
    async fn resolves_into_folding_analysis() {
        let graph = single_stack_graph();
        // Keep base and leaf; mid folds into leaf.
        let s = spec(|s| s.keep = vec!["base".into(), "leaf".into()]);
        let result = resolve(&graph, &s).await.unwrap();

        let analysis =
            crate::submit::analysis_from_selection(&result.path, &result.assignments, "main")
                .unwrap();
        assert_eq!(analysis.segments.len(), 2);
        assert_eq!(analysis.segments[0].bookmark_names, vec!["base"]);
        assert_eq!(analysis.segments[1].bookmark_names, vec!["leaf"]);
        // mid's commit folded into leaf's segment (newest first).
        let leaf_ids: Vec<&str> = analysis.segments[1]
            .commits
            .iter()
            .map(|c| c.change_id.as_str())
            .collect();
        assert_eq!(leaf_ids, vec!["cccc3333", "bbbb2222"]);
    }

    // -- SelectionSpec::from_args --

    fn parse_submit(args: &[&str]) -> crate::cli::submit::SubmitArgs {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(args).unwrap();
        match cli.command {
            Some(crate::cli::Commands::Submit(a)) => *a,
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn from_args_splits_rev_name() {
        let args = parse_submit(&["stakk", "submit", "--new", "abc=my-name", "--new", "def"]);
        let spec = SelectionSpec::from_args(&args).unwrap();
        assert_eq!(
            spec.new,
            vec![
                NewBookmarkSpec {
                    rev: "abc".into(),
                    name: Some("my-name".into()),
                },
                NewBookmarkSpec {
                    rev: "def".into(),
                    name: None,
                },
            ],
        );
    }

    #[test]
    fn from_args_rejects_empty_rev_or_name() {
        for bad in ["=name", "rev=", ""] {
            let args = parse_submit(&["stakk", "submit", "--new", bad]);
            let err = SelectionSpec::from_args(&args).unwrap_err();
            assert!(
                matches!(err, ExplicitSelectionError::InvalidNewSpec { .. }),
                "expected InvalidNewSpec for {bad:?}",
            );
        }
    }

    #[test]
    fn from_args_validates_explicit_names() {
        let args = parse_submit(&["stakk", "submit", "--new", "abc=has space"]);
        let err = SelectionSpec::from_args(&args).unwrap_err();
        assert!(matches!(
            err,
            ExplicitSelectionError::Gen(BookmarkGenError::InvalidName { .. })
        ));
    }

    #[test]
    fn spec_is_empty() {
        let args = parse_submit(&["stakk", "submit"]);
        assert!(SelectionSpec::from_args(&args).unwrap().is_empty());
        let args = parse_submit(&["stakk", "submit", "--keep-all"]);
        assert!(!SelectionSpec::from_args(&args).unwrap().is_empty());
    }
}
