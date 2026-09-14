//! The black-box end-to-end harness: a Forgejo instance the lifecycle script
//! started, a temporary jj repository wired to a fresh repository on it, and
//! a runner for the `stakk` binary under test.
//!
//! Nothing here imports from `src/`: the assertions must not share code with
//! the forge under test, so the API client is a second, independent one.

pub mod forgejo;
pub mod jj_repo;
pub mod stakk;

use std::hash::BuildHasher;
use std::hash::Hasher;
use std::hash::RandomState;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub use forgejo::ForgejoApi;
pub use forgejo::Pull;
pub use jj_repo::JjRepo;
pub use stakk::Stakk;

/// The instance the suite runs against, from the three `STAKK_E2E_*`
/// variables the lifecycle script writes to `.e2e-forgejo.env`.
#[derive(Debug, Clone)]
pub struct Env {
    /// Instance URL, e.g. `http://localhost:3000`.
    pub url: String,
    /// The user that owns every repository the suite creates.
    pub user: String,
    pub token: String,
    /// The URL's authority (`localhost:3000`): the host stakk sees in the
    /// remote URL, and what a test maps with `STAKK_HOSTS`.
    pub host: String,
}

impl Env {
    /// Reads the environment. A missing variable is a failure, not a skip:
    /// the `e2e` nextest profile is opt-in, so running it without an instance
    /// is an operator error.
    pub fn load() -> Self {
        let url = required("STAKK_E2E_FORGEJO_URL");
        let user = required("STAKK_E2E_FORGEJO_USER");
        let token = required("STAKK_E2E_FORGEJO_TOKEN");
        let host = authority(&url);
        Self {
            url,
            user,
            token,
            host,
        }
    }

    pub fn api(&self) -> ForgejoApi {
        ForgejoApi::new(self)
    }

    /// `http://<user>:<token>@<host>/<user>/<repo>.git` — the push URL jj
    /// uses. stakk reads the same remote and strips the credentials.
    pub fn remote_url(&self, repo: &str) -> String {
        format!(
            "http://{}:{}@{}/{}/{}.git",
            self.user, self.token, self.host, self.user, repo
        )
    }
}

fn required(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => panic!(
            "\n\n{name} is not set.\n\nThe e2e suite needs a running Forgejo instance. Start one \
             with `mise run e2e:up`, then either run `mise run e2e` or export the environment \
             with `eval \"$(scripts/e2e-forgejo.py env)\"` and run `cargo nextest run --profile \
             e2e`.\n\n"
        ),
    }
}

/// `http://localhost:3000/` -> `localhost:3000`.
fn authority(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// A repository name unique to this test run: `e2e-<test>-<hex>`. Repos are
/// never deleted; the instance is ephemeral, and leftovers are what you look
/// at when a test fails.
pub fn unique_repo_name(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // `RandomState` is randomly keyed per process; mixing in the clock keeps
    // two processes started in the same instant apart as well.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    let hex = format!("{:016x}", hasher.finish());
    format!("e2e-{test}-{}", &hex[..10])
}

/// One test's world: a fresh Forgejo repository, a temporary jj repository
/// pushed to it, and a `stakk` runner pointed at that repository.
pub struct Scenario {
    pub env: Env,
    pub api: ForgejoApi,
    /// The repository name on the instance (the owner is `env.user`).
    pub repo_name: String,
    pub repo: JjRepo,
    pub stakk: Stakk,
}

impl Scenario {
    /// Creates the Forgejo repository `e2e-<test>-<hex>`, seeds a jj
    /// repository with an initial commit on `main`, pushes it and waits until
    /// the instance reports the repository non-empty.
    pub async fn new(test: &str) -> Self {
        let env = Env::load();
        let api = env.api();
        let repo_name = unique_repo_name(test);
        api.create_repo(&repo_name).await;
        let repo = JjRepo::new(&env, &api, &repo_name).await;
        let stakk = Stakk::new(&env, &repo);
        Self {
            env,
            api,
            repo_name,
            repo,
            stakk,
        }
    }

    /// `main <- a(feat-a) <- b(feat-b)`, with two-line descriptions so the PR
    /// title (first line) and body (the rest) are both observable. Leaves an
    /// empty `@` on top of `b`. Returns the change ids of `a` and `b`.
    pub fn seed_feat_a_feat_b(&self) -> (String, String) {
        let a = self
            .repo
            .commit("main", "feat a\n\nBody of a.", &[("a.txt", "a\n")]);
        let b = self
            .repo
            .commit(&a, "feat b\n\nBody of b.", &[("b.txt", "b\n")]);
        self.repo.bookmark("feat-a", &a);
        self.repo.bookmark("feat-b", &b);
        self.repo.new_empty_head(&b);
        (a, b)
    }

    pub async fn open_pulls(&self) -> Vec<Pull> {
        self.api.list_pulls(&self.repo_name, "open").await
    }

    /// The single open PR whose head branch is `head`; panics on zero or
    /// several.
    pub async fn pull_for(&self, head: &str) -> Pull {
        let pulls = self.open_pulls().await;
        let mut matching: Vec<Pull> = pulls.into_iter().filter(|p| p.head.r#ref == head).collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one open PR with head {head}, found {matching:#?}"
        );
        matching.remove(0)
    }

    /// The comments on PR `number` that carry stakk's stack marker.
    pub async fn stakk_comments(&self, number: u64) -> Vec<StackComment> {
        self.api
            .list_issue_comments(&self.repo_name, number)
            .await
            .into_iter()
            .filter_map(|c| {
                Some(StackComment {
                    id: c.id,
                    body: c.body?,
                })
            })
            .filter(|c| c.body.contains(STACK_COMMENT_MARKER))
            .collect()
    }

    pub async fn remote_branch_exists(&self, name: &str) -> bool {
        self.api.get_branch(&self.repo_name, name).await.is_some()
    }
}

/// A comment stakk wrote: the marker is on its first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackComment {
    pub id: u64,
    pub body: String,
}

/// The first line of every stack comment stakk writes.
pub const STACK_COMMENT_MARKER: &str = "<!--- STAKK_STACK: ";
/// The HTML-comment fence around stack info placed in a PR body.
pub const BODY_FENCE_START: &str = "<!-- STAKK_BODY_START -->";
pub const BODY_FENCE_END: &str = "<!-- STAKK_BODY_END -->";
