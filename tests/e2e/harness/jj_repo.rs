//! A temporary jj repository wired to a repository on the Forgejo instance.
//!
//! Every operation is a `jj` subprocess; there is no `git` call anywhere in
//! the harness. The repository is isolated from the developer's jj
//! configuration through `JJ_CONFIG`, and the stakk config it carries sets
//! `inherit = false`, which is the only thing that keeps the developer's
//! `~/.config/stakk/config.toml` out of a run.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

use tempfile::TempDir;

use super::Env;
use super::ForgejoApi;

/// How long the first push may take to become visible through the API. The
/// instance keeps answering `empty: true` for a while after it.
const READINESS_TIMEOUT: Duration = Duration::from_secs(60);

pub struct JjRepo {
    /// Owns the directory; dropped with the repo.
    _dir: TempDir,
    /// The jj workspace root — cwd of every `jj` and `stakk` run.
    pub workspace: PathBuf,
    /// `JJ_CONFIG` for jj runs, the harness's and stakk's alike.
    pub jj_config: PathBuf,
    /// `STAKK_CONFIG`: `inherit = false` and nothing else.
    pub stakk_config: PathBuf,
}

impl JjRepo {
    /// Initialises the repository, adds `origin`, commits a README on
    /// `main`, pushes it, and waits for the instance to report the
    /// repository non-empty. `@` is left as an empty change on `main`.
    pub async fn new(env: &Env, api: &ForgejoApi, repo_name: &str) -> Self {
        let dir = TempDir::with_prefix("stakk-e2e-").expect("temp dir");
        let workspace = dir.path().join("repo");
        fs::create_dir(&workspace).expect("workspace dir");

        let jj_config = dir.path().join("jj.toml");
        fs::write(
            &jj_config,
            "[user]\nname = \"stakk e2e\"\nemail = \"stakk@example.invalid\"\n\n[ui]\npaginate = \
             \"never\"\n",
        )
        .expect("jj config");

        let stakk_config = dir.path().join("stakk.toml");
        fs::write(&stakk_config, "inherit = false\n").expect("stakk config");

        let repo = Self {
            _dir: dir,
            workspace,
            jj_config,
            stakk_config,
        };

        repo.jj(&["git", "init"]);
        repo.jj(&["git", "remote", "add", "origin", &env.remote_url(repo_name)]);
        repo.write_files(&[("README.md", "# e2e\n")]);
        repo.jj(&["commit", "-m", "Initial commit"]);
        repo.jj(&["bookmark", "create", "main", "-r", "@-"]);
        repo.push("main");
        repo.await_first_push(api, repo_name).await;
        repo
    }

    /// Polls `GET /repos/{owner}/{repo}` until `empty` is false. Later
    /// pushes of new branches are visible immediately; only the first one
    /// into an empty repository lags.
    async fn await_first_push(&self, api: &ForgejoApi, repo_name: &str) {
        let started = Instant::now();
        loop {
            let repo = api.get_repo(repo_name).await;
            if !repo.empty {
                assert_eq!(repo.default_branch, "main");
                return;
            }
            assert!(
                started.elapsed() < READINESS_TIMEOUT,
                "{repo_name} still reports empty {READINESS_TIMEOUT:?} after pushing main"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Runs `jj` in the workspace and returns stdout; panics on failure with
    /// both streams.
    pub fn jj(&self, args: &[&str]) -> String {
        let output = Command::new("jj")
            .args(["--config", "ui.paginate=never"])
            .args(args)
            .current_dir(&self.workspace)
            .env("JJ_CONFIG", &self.jj_config)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("spawn jj");
        assert!(
            output.status.success(),
            "jj {args:?} failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).expect("jj stdout is UTF-8")
    }

    fn write_files(&self, files: &[(&str, &str)]) {
        for (path, content) in files {
            let full = self.workspace.join(path);
            if let Some(parent) = Path::new(&full).parent() {
                fs::create_dir_all(parent).expect("file parent dir");
            }
            fs::write(&full, content).expect("write file");
        }
    }

    /// `jj new <parent> -m <message>` plus `files` in the working copy.
    /// Returns the new change id. The files are snapshotted by the `jj log`
    /// that reads the id, so the returned change already contains them.
    pub fn commit(&self, parent: &str, message: &str, files: &[(&str, &str)]) -> String {
        self.jj(&["new", parent, "-m", message]);
        self.write_files(files);
        self.change_id_of("@")
    }

    pub fn bookmark(&self, name: &str, change_id: &str) {
        self.jj(&["bookmark", "create", name, "-r", change_id]);
    }

    pub fn describe(&self, change_id: &str, message: &str) {
        self.jj(&["describe", "-r", change_id, "-m", message]);
    }

    /// `jj rebase -s <source> -d <destination>`: moves `source` and its
    /// descendants.
    pub fn rebase(&self, source: &str, destination: &str) {
        self.jj(&["rebase", "-s", source, "-d", destination]);
    }

    pub fn push(&self, bookmark: &str) {
        self.jj(&["git", "push", "--remote", "origin", "--bookmark", bookmark]);
    }

    /// Leaves `@` as an empty, undescribed change on top of `parent`, the
    /// way a real session looks after the last commit.
    pub fn new_empty_head(&self, parent: &str) {
        self.jj(&["new", parent]);
    }

    /// Local bookmark names, deleted-but-tracked ones excluded.
    pub fn bookmark_names(&self) -> Vec<String> {
        self.jj(&["bookmark", "list", "-T", r#"if(present, name ++ "\n")"#])
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn change_id_of(&self, rev: &str) -> String {
        self.jj(&["log", "-r", rev, "--no-graph", "-T", "change_id"])
    }

    pub fn commit_id_of(&self, rev: &str) -> String {
        self.jj(&["log", "-r", rev, "--no-graph", "-T", "commit_id"])
    }

    /// The number of commits `revset` selects.
    pub fn count(&self, revset: &str) -> usize {
        self.jj(&[
            "log",
            "-r",
            revset,
            "--no-graph",
            "-T",
            r#"commit_id ++ "\n""#,
        ])
        .lines()
        .count()
    }
}
