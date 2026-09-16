//! `stakk graph` against real jj output, G1–G3 in the plan. The command is
//! offline, but `trunk()` needs a remote, and the Forgejo repository is
//! exactly that.

use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::harness::Scenario;

/// `main <- a(feat-a) <- b(feat-b)` and a sibling `main <- c(feat-c)`, `c`
/// committed in a later second than `b`. Leaves an empty `@` on top of `c`.
fn seed_two_stacks(s: &Scenario) -> (String, String, String) {
    let (a, b) = s.seed_feat_a_feat_b();
    // `committer_timestamp` has second resolution, and two stacks committed
    // within the same second are ordered by change id instead, so the seed
    // makes the timestamps differ for real.
    thread::sleep(Duration::from_millis(1100));
    let c = s
        .repo
        .commit("main", "feat c\n\nBody of c.", &[("c.txt", "c\n")]);
    s.repo.bookmark("feat-c", &c);
    s.repo.new_empty_head(&c);
    (a, b, c)
}

/// Bookmark names per segment, trunk-first, for one `stacks[]` entry.
fn segment_bookmarks(stack: &Value) -> Vec<Vec<String>> {
    stack["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|segment| {
            segment["bookmarks"]
                .as_array()
                .expect("bookmarks")
                .iter()
                .map(|b| b["name"].as_str().expect("name").to_string())
                .collect()
        })
        .collect()
}

/// Every key path in `sparse` exists in `full` with the same value. Arrays
/// must match element for element; objects may have extra keys in `full`.
fn assert_subset(sparse: &Value, full: &Value, path: &str) {
    match (sparse, full) {
        (Value::Object(s), Value::Object(f)) => {
            for (key, value) in s {
                let child = format!("{path}.{key}");
                let counterpart = f
                    .get(key)
                    .unwrap_or_else(|| panic!("{child} is in sparse but not in full"));
                assert_subset(value, counterpart, &child);
            }
        }
        (Value::Array(s), Value::Array(f)) => {
            assert_eq!(s.len(), f.len(), "{path} differs in length");
            for (i, (sv, fv)) in s.iter().zip(f).enumerate() {
                assert_subset(sv, fv, &format!("{path}[{i}]"));
            }
        }
        (s, f) => assert_eq!(s, f, "{path} differs"),
    }
}

#[tokio::test]
async fn g01_json_shape_on_real_jj_output() {
    let s = Scenario::new("g01").await;
    let (a, b, c) = seed_two_stacks(&s);

    let doc = s.stakk.graph_json("json-full");

    assert_eq!(doc["schema_version"], 3);
    assert_eq!(doc["default_branch"], "main");
    let remotes = doc["remotes"].as_array().expect("remotes");
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0]["name"], "origin");
    // The host is whatever the harness's remote URL carries, port included.
    assert_eq!(remotes[0]["host"], s.env.host);
    assert_eq!(
        remotes[0]["repo"],
        format!("{}/{}", s.env.user, s.repo_name)
    );
    assert!(
        remotes[0]["url"]
            .as_str()
            .expect("url")
            .ends_with(&format!("/{}/{}.git", s.env.user, s.repo_name)),
        "{}",
        remotes[0]["url"]
    );
    assert_eq!(doc["excluded_bookmarks"], Value::Array(vec![]));
    assert_eq!(doc["excluded_heads"], Value::Array(vec![]));

    let stacks = doc["stacks"].as_array().expect("stacks");
    assert_eq!(stacks.len(), 2, "{doc:#}");
    let mut shapes: Vec<Vec<Vec<String>>> = stacks.iter().map(segment_bookmarks).collect();
    shapes.sort();
    assert_eq!(
        shapes,
        vec![
            vec![vec!["feat-a".to_string()], vec!["feat-b".to_string()]],
            vec![vec!["feat-c".to_string()]],
        ]
    );

    // Every commit is one of the three seeded ones, fully described.
    let mut seen = Vec::new();
    for stack in stacks {
        for segment in stack["segments"].as_array().expect("segments") {
            let commits = segment["commits"].as_array().expect("commits");
            assert_eq!(commits.len(), 1, "one commit per segment: {segment:#}");
            let commit = &commits[0];
            seen.push(commit["change_id"].as_str().expect("change_id").to_string());
            assert!(commit["commit_id"].is_string(), "{commit:#}");
            assert!(commit["description"].is_string(), "{commit:#}");
            assert_eq!(commit["author"]["email"], "stakk@example.invalid");
            assert!(commit["files"].is_array(), "{commit:#}");
            assert_eq!(commit["is_immutable"], false);
            assert_eq!(commit["is_boundary"], true);
            let title = commit["title"].as_str().expect("title");
            // jj stores descriptions with a trailing newline.
            assert_eq!(
                commit["description"]
                    .as_str()
                    .expect("description")
                    .trim_end(),
                format!("{title}\n\nBody of {}.", &title[5..])
            );
        }
    }
    seen.sort();
    let mut expected = vec![a, b, c];
    expected.sort();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn g02_sparse_is_a_subset_of_full() {
    let s = Scenario::new("g02").await;
    seed_two_stacks(&s);

    let sparse = s.stakk.graph_json("json");
    let full = s.stakk.graph_json("json-full");

    assert_subset(&sparse, &full, "$");
    // And sparse really is sparse: the full-only fields are absent.
    let commit = &sparse["stacks"][0]["segments"][0]["commits"][0];
    for key in ["commit_id", "description", "author", "files"] {
        assert!(
            commit.get(key).is_none(),
            "{key} leaked into sparse: {commit:#}"
        );
    }
    for key in [
        "change_id",
        "short_change_id",
        "title",
        "committer_timestamp",
    ] {
        assert!(
            commit.get(key).is_some(),
            "{key} missing from sparse: {commit:#}"
        );
    }
}

#[tokio::test]
async fn g03_stack_order_newest_first() {
    let s = Scenario::new("g03").await;
    seed_two_stacks(&s);

    let doc = s.stakk.graph_json("json");

    let stacks = doc["stacks"].as_array().expect("stacks");
    assert_eq!(stacks.len(), 2);
    assert_eq!(
        segment_bookmarks(&stacks[0]),
        vec![vec!["feat-c".to_string()]],
        "the stack committed last comes first: {doc:#}"
    );
    assert_eq!(
        segment_bookmarks(&stacks[1]),
        vec![vec!["feat-a".to_string()], vec!["feat-b".to_string()]]
    );
}
