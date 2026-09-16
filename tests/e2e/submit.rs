//! Forge-agnostic submission scenarios, S1–S13 in the plan. Each test creates
//! its own Forgejo repository and jj repository, runs the `stakk` binary
//! with selection flags, and asserts through the Forgejo API and jj.
//!
//! The harness names no forge, so every run below reaches the instance
//! through the network probe unless the test says otherwise (S12, S13).

use crate::harness::BODY_FENCE_END;
use crate::harness::BODY_FENCE_START;
use crate::harness::Pull;
use crate::harness::Scenario;

const KEEP_A_B: &[&str] = &["--keep", "feat-a", "--keep", "feat-b"];

/// The shape S1 produces: two open PRs, `feat-a` on `main` and `feat-b` on
/// `feat-a`, commit-derived titles and bodies, both remote branches present,
/// and exactly one stack comment per PR naming both PR URLs.
async fn assert_two_pr_stack(s: &Scenario) -> (Pull, Pull) {
    let pulls = s.open_pulls().await;
    assert_eq!(pulls.len(), 2, "expected two open PRs, got {pulls:#?}");
    let a = s.pull_for("feat-a").await;
    let b = s.pull_for("feat-b").await;

    assert_eq!(a.base.r#ref, "main");
    assert_eq!(b.base.r#ref, "feat-a");
    assert_eq!(a.state, "open");
    assert_eq!(b.state, "open");
    assert_eq!(a.title, "feat a");
    assert_eq!(b.title, "feat b");
    assert_eq!(a.body_text().trim(), "Body of a.");
    assert_eq!(b.body_text().trim(), "Body of b.");

    assert!(s.remote_branch_exists("feat-a").await);
    assert!(s.remote_branch_exists("feat-b").await);

    for pr in [&a, &b] {
        let comments = s.stakk_comments(pr.number).await;
        assert_eq!(
            comments.len(),
            1,
            "PR #{} should carry exactly one stack comment: {comments:#?}",
            pr.number
        );
        let body = &comments[0].body;
        assert!(body.contains(&a.html_url), "{body}");
        assert!(body.contains(&b.html_url), "{body}");
    }
    (a, b)
}

/// The text of `body` with the stack fence cut out, and how many fences it
/// had. A body without a fence comes back unchanged with a count of zero.
fn split_fence(body: &str) -> (String, usize) {
    let starts = body.matches(BODY_FENCE_START).count();
    let ends = body.matches(BODY_FENCE_END).count();
    assert_eq!(starts, ends, "unbalanced fence in body:\n{body}");
    let Some(start) = body.find(BODY_FENCE_START) else {
        return (body.to_string(), 0);
    };
    let end = body.find(BODY_FENCE_END).expect("fence end") + BODY_FENCE_END.len();
    assert!(start < end, "fence end before start in body:\n{body}");
    (format!("{}{}", &body[..start], &body[end..]), starts)
}

fn fenced(body: &str) -> &str {
    let start = body.find(BODY_FENCE_START).expect("fence start");
    let end = body.find(BODY_FENCE_END).expect("fence end");
    &body[start..end]
}

/// What the probe prints once it has recognised the instance.
fn detected_hint(s: &Scenario) -> String {
    format!("Detected Forgejo at {}", s.env.host)
}

#[tokio::test]
async fn s01_two_bookmark_stack_from_scratch() {
    let s = Scenario::new("s01").await;
    s.seed_feat_a_feat_b();

    let output = s.stakk.submit_output(KEEP_A_B);

    // Nothing named the forge, so the probe ran — and said so on stderr,
    // where a scripted run can read it.
    assert!(
        output.stderr.contains(&detected_hint(&s)),
        "{}",
        output.stderr
    );
    assert_two_pr_stack(&s).await;
}

#[tokio::test]
async fn s02_idempotent_rerun() {
    let s = Scenario::new("s02").await;
    s.seed_feat_a_feat_b();
    s.stakk.submit(KEEP_A_B);
    let (a1, b1) = assert_two_pr_stack(&s).await;
    let comments_on_a = s.stakk_comments(a1.number).await;
    let comments_on_b = s.stakk_comments(b1.number).await;

    s.stakk.submit(KEEP_A_B);

    let (a2, b2) = assert_two_pr_stack(&s).await;
    assert_eq!(a1.number, a2.number);
    assert_eq!(b1.number, b2.number);
    assert_eq!(s.api.list_pulls(&s.repo_name, "all").await.len(), 2);
    // The same comment, by id, with the same body: updated in place or left
    // alone, never deleted and recreated.
    assert_eq!(s.stakk_comments(a2.number).await, comments_on_a);
    assert_eq!(s.stakk_comments(b2.number).await, comments_on_b);
}

#[tokio::test]
async fn s03_new_bookmark_by_name() {
    let s = Scenario::new("s03").await;
    let a = s
        .repo
        .commit("main", "feat a\n\nBody of a.", &[("a.txt", "a\n")]);
    let b = s
        .repo
        .commit(&a, "feat b\n\nBody of b.", &[("b.txt", "b\n")]);
    s.repo.new_empty_head(&b);
    assert!(s.repo.bookmark_names().iter().all(|n| n == "main"));

    let new_a = format!("{a}=feat-a");
    let new_b = format!("{b}=feat-b");
    s.stakk.submit(&["--new", &new_a, "--new", &new_b]);

    assert_eq!(s.repo.change_id_of("feat-a"), a);
    assert_eq!(s.repo.change_id_of("feat-b"), b);
    assert_two_pr_stack(&s).await;
}

/// `--new-auto` derives a TF-IDF name from the description and files and
/// falls back to `stakk-<change_id>` only when nothing can be derived, so the
/// name is read from the plan stakk prints rather than predicted.
#[tokio::test]
async fn s04_new_bookmark_generated_name() {
    let s = Scenario::new("s04").await;
    let a = s.repo.commit(
        "main",
        "Add parser for config files\n\nBody of a.",
        &[("parser.rs", "fn parse() {}\n")],
    );
    s.repo.new_empty_head(&a);

    let stdout = s.stakk.submit(&["--new-auto", &a]);

    let name = stdout
        .lines()
        .find_map(|line| {
            let rest = line.trim_start().strip_prefix("Create bookmark ")?;
            let (name, _) = rest.split_once(" at ")?;
            Some(name.to_string())
        })
        .unwrap_or_else(|| panic!("no `Create bookmark <name> at <id>` line in:\n{stdout}"));
    assert!(!name.is_empty());

    let local = s.repo.bookmark_names();
    assert!(local.contains(&name), "{local:?} lacks {name}");
    assert_eq!(local.len(), 2, "main plus the new one: {local:?}");
    assert_eq!(s.repo.change_id_of(&name), a);
    let mut remote: Vec<String> = s
        .api
        .list_branches(&s.repo_name)
        .await
        .into_iter()
        .map(|b| b.name)
        .collect();
    remote.sort();
    let mut expected = vec!["main".to_string(), name.clone()];
    expected.sort();
    assert_eq!(remote, expected);
    let pulls = s.open_pulls().await;
    assert_eq!(pulls.len(), 1, "{pulls:#?}");
    assert_eq!(pulls[0].head.r#ref, name);
    assert_eq!(pulls[0].base.r#ref, "main");
}

#[tokio::test]
async fn s05_folding() {
    let s = Scenario::new("s05").await;
    let a = s
        .repo
        .commit("main", "feat a\n\nBody of a.", &[("a.txt", "a\n")]);
    let b = s
        .repo
        .commit(&a, "feat b\n\nBody of b.", &[("b.txt", "b\n")]);
    let c = s
        .repo
        .commit(&b, "feat c\n\nBody of c.", &[("c.txt", "c\n")]);
    s.repo.bookmark("feat-c", &c);
    s.repo.new_empty_head(&c);

    s.stakk.submit(&["--keep", "feat-c"]);

    let pulls = s.open_pulls().await;
    assert_eq!(pulls.len(), 1, "{pulls:#?}");
    let pr = &pulls[0];
    assert_eq!(pr.head.r#ref, "feat-c");
    assert_eq!(pr.base.r#ref, "main");
    // Three commits fold into the one PR: the pushed head is `c` itself and
    // `main..feat-c` is all three.
    let branch = s
        .api
        .get_branch(&s.repo_name, "feat-c")
        .await
        .expect("feat-c pushed");
    assert_eq!(branch.commit.id, s.repo.commit_id_of("feat-c"));
    assert_eq!(s.repo.count("main..feat-c"), 3);
    // A folded PR is titled after its boundary commit, the tip, and its body
    // joins every folded description tip-first with `---` separators.
    assert_eq!(pr.title, "feat c");
    let body = pr.body_text();
    let pos = |part: &str| {
        body.find(part)
            .unwrap_or_else(|| panic!("{part:?} missing from body:\n{body}"))
    };
    assert!(pos("Body of c.") < pos("Body of b."), "{body}");
    assert!(pos("Body of b.") < pos("Body of a."), "{body}");
    assert_eq!(body.matches("---").count(), 2, "{body}");
    assert!(
        s.stakk_comments(pr.number).await.is_empty(),
        "a single PR is not a stack"
    );
}

/// Only bases and comments are asserted. Forgejo does not auto-close a PR
/// whose head becomes an ancestor of its base, so the interleaving rule that
/// motivates stakk's execute order (push, retarget, create — one bookmark at
/// a time) is not observable here; a green run says nothing about it.
#[tokio::test]
async fn s06_insert_in_the_middle() {
    let s = Scenario::new("s06").await;
    let (a, b) = s.seed_feat_a_feat_b();
    s.stakk.submit(KEEP_A_B);
    let (pr_a, pr_b) = assert_two_pr_stack(&s).await;

    let c = s
        .repo
        .commit(&a, "feat c\n\nBody of c.", &[("c.txt", "c\n")]);
    s.repo.bookmark("feat-c", &c);
    s.repo.rebase(&b, &c);

    s.stakk
        .submit(&["--keep", "feat-a", "--keep", "feat-c", "--keep", "feat-b"]);

    let pulls = s.open_pulls().await;
    assert_eq!(pulls.len(), 3, "{pulls:#?}");
    let new_a = s.pull_for("feat-a").await;
    let new_c = s.pull_for("feat-c").await;
    let new_b = s.pull_for("feat-b").await;
    assert_eq!(new_a.number, pr_a.number);
    assert_eq!(new_b.number, pr_b.number);
    assert_eq!(new_a.base.r#ref, "main");
    assert_eq!(new_c.base.r#ref, "feat-a");
    assert_eq!(new_b.base.r#ref, "feat-c");

    for pr in [&new_a, &new_c, &new_b] {
        let comments = s.stakk_comments(pr.number).await;
        assert_eq!(comments.len(), 1, "PR #{}: {comments:#?}", pr.number);
        let body = &comments[0].body;
        // Leaf first, like `stakk graph`: b above c above a.
        let pos = |url: &str| {
            body.find(url)
                .unwrap_or_else(|| panic!("{url} missing:\n{body}"))
        };
        assert!(pos(&new_b.html_url) < pos(&new_c.html_url), "{body}");
        assert!(pos(&new_c.html_url) < pos(&new_a.html_url), "{body}");
    }
}

#[tokio::test]
async fn s07_body_placement() {
    let s = Scenario::new("s07").await;
    s.seed_feat_a_feat_b();

    s.stakk.submit(&[
        "--keep",
        "feat-a",
        "--keep",
        "feat-b",
        "--stack-placement",
        "body",
    ]);

    let a = s.pull_for("feat-a").await;
    let b = s.pull_for("feat-b").await;
    for (pr, original) in [(&a, "Body of a."), (&b, "Body of b.")] {
        assert!(s.stakk_comments(pr.number).await.is_empty());
        let body = pr.body_text();
        let (outside, fences) = split_fence(body);
        assert_eq!(fences, 1, "{body}");
        assert_eq!(outside.trim(), original, "{body}");
        let inside = fenced(body);
        assert!(inside.contains(&a.html_url), "{body}");
        assert!(inside.contains(&b.html_url), "{body}");
    }
}

#[tokio::test]
async fn s08_placement_migration_and_cleanup() {
    let s = Scenario::new("s08").await;
    s.seed_feat_a_feat_b();
    s.stakk.submit(KEEP_A_B);
    let (a, b) = assert_two_pr_stack(&s).await;

    s.stakk.submit(&[
        "--keep",
        "feat-a",
        "--keep",
        "feat-b",
        "--stack-placement",
        "body",
    ]);
    for pr in [&a, &b] {
        assert!(
            s.stakk_comments(pr.number).await.is_empty(),
            "PR #{}",
            pr.number
        );
        let body = s.api.get_pull(&s.repo_name, pr.number).await;
        assert_eq!(split_fence(body.body_text()).1, 1, "{}", body.body_text());
    }

    s.stakk.submit(&[
        "--keep",
        "feat-a",
        "--keep",
        "feat-b",
        "--stack-placement",
        "none",
    ]);
    for (pr, original) in [(&a, "Body of a."), (&b, "Body of b.")] {
        assert!(
            s.stakk_comments(pr.number).await.is_empty(),
            "PR #{}",
            pr.number
        );
        let after = s.api.get_pull(&s.repo_name, pr.number).await;
        assert_eq!(split_fence(after.body_text()).1, 0, "{}", after.body_text());
        assert_eq!(after.body_text().trim(), original);
    }
}

#[tokio::test]
async fn s09_content_sync() {
    let s = Scenario::new("s09").await;
    let (a, _) = s.seed_feat_a_feat_b();
    s.stakk.submit(KEEP_A_B);
    let (pr_a, pr_b) = assert_two_pr_stack(&s).await;

    s.repo.describe(&a, "feat a renamed\n\nNew body of a.");

    s.stakk.submit(KEEP_A_B);
    let unsynced = s.api.get_pull(&s.repo_name, pr_a.number).await;
    assert_eq!(unsynced.title, "feat a");
    assert_eq!(unsynced.body_text().trim(), "Body of a.");

    s.stakk.submit(&[
        "--keep",
        "feat-a",
        "--keep",
        "feat-b",
        "--sync-pr-content",
        "all",
    ]);
    let synced = s.api.get_pull(&s.repo_name, pr_a.number).await;
    assert_eq!(synced.title, "feat a renamed");
    assert_eq!(synced.body_text().trim(), "New body of a.");

    for pr in [&pr_a, &pr_b] {
        assert_eq!(
            s.stakk_comments(pr.number).await.len(),
            1,
            "PR #{}",
            pr.number
        );
    }
}

#[tokio::test]
async fn s10_dry_run() {
    let s = Scenario::new("s10").await;
    let a = s
        .repo
        .commit("main", "feat a\n\nBody of a.", &[("a.txt", "a\n")]);
    s.repo.new_empty_head(&a);

    let new_a = format!("{a}=feat-a");
    let stdout = s.stakk.submit(&["--new", &new_a, "--dry-run"]);

    assert!(stdout.contains("DRY RUN"), "{stdout}");
    assert!(stdout.contains("Create bookmark feat-a at"), "{stdout}");
    assert_eq!(s.repo.bookmark_names(), vec!["main".to_string()]);
    assert!(!s.remote_branch_exists("feat-a").await);
    assert!(s.api.list_pulls(&s.repo_name, "all").await.is_empty());
}

/// The one place the suite touches `--native-stacks`, because `auto` is the
/// intended future default: on a forge without native stacks the reconcile
/// must resolve `auto-comment` to writing comments, not to a failure and not
/// to silence.
#[tokio::test]
async fn s11_native_stacks_auto_on_forgejo() {
    let s = Scenario::new("s11").await;
    s.seed_feat_a_feat_b();

    s.stakk.submit(&[
        "--keep",
        "feat-a",
        "--keep",
        "feat-b",
        "--native-stacks",
        "auto",
    ]);

    assert_two_pr_stack(&s).await;
}

/// A host table entry skips the probe: the run is a plain submission with no
/// detection hint.
#[tokio::test]
async fn s12_hosts_table_skips_the_probe() {
    let mut s = Scenario::new("s12").await;
    let rule = format!("{}=forgejo", s.env.host);
    s.stakk.set_env("STAKK_HOSTS", &rule);
    s.seed_feat_a_feat_b();

    let output = s.stakk.submit_output(KEEP_A_B);

    assert!(
        !output.stderr.contains(&detected_hint(&s)),
        "{}",
        output.stderr
    );
    assert_two_pr_stack(&s).await;
}

/// An explicit forge skips the probe too, host table or not.
#[tokio::test]
async fn s13_explicit_forge_skips_the_probe() {
    let mut s = Scenario::new("s13").await;
    s.stakk.set_env("STAKK_FORGE", "forgejo");
    s.seed_feat_a_feat_b();

    let output = s.stakk.submit_output(KEEP_A_B);

    assert!(
        !output.stderr.contains(&detected_hint(&s)),
        "{}",
        output.stderr
    );
    assert_two_pr_stack(&s).await;
}
