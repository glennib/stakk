//! Runs the `stakk` binary under test inside a [`JjRepo`].
//!
//! The child's environment is scrubbed of everything a developer's shell
//! could leak into a run — `STAKK_*`, `GH_*`, `GITHUB_*`, `FORGEJO_TOKEN` —
//! before the harness's own variables go in. Stdin is null, so a submission
//! without selection flags fails loudly instead of waiting on a TUI.
//!
//! The forge is deliberately *not* named: no `STAKK_FORGE`, no `STAKK_HOSTS`.
//! The local instance is an unknown `host:port` to stakk, so every default
//! run goes through the network probe, and the two scenarios that name the
//! forge add their variable with [`Stakk::set_env`].

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;

use super::Env;
use super::JjRepo;

pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub status: ExitStatus,
}

pub struct Stakk {
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

impl Stakk {
    pub fn new(env: &Env, repo: &JjRepo) -> Self {
        let vars = vec![
            ("NO_COLOR".to_string(), "1".to_string()),
            ("FORGEJO_TOKEN".to_string(), env.token.clone()),
            (
                "STAKK_CONFIG".to_string(),
                repo.stakk_config.to_string_lossy().into_owned(),
            ),
            (
                "JJ_CONFIG".to_string(),
                repo.jj_config.to_string_lossy().into_owned(),
            ),
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ];
        Self {
            cwd: repo.workspace.clone(),
            env: vars,
        }
    }

    /// Add one variable for this runner's later runs, after the scrub.
    pub fn set_env(&mut self, name: &str, value: &str) {
        self.env.push((name.to_string(), value.to_string()));
    }

    pub fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stakk"));
        command
            .args(args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if is_leaky(&name) {
                command.env_remove(&name);
            }
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let output = command.output().expect("spawn stakk");
        Output {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status,
        }
    }

    /// `stakk submit <args>`; asserts success and returns stdout.
    pub fn submit(&self, args: &[&str]) -> String {
        self.submit_output(args).stdout
    }

    /// `stakk submit <args>`; asserts success and returns both streams, for
    /// a test that reads what went to stderr.
    pub fn submit_output(&self, args: &[&str]) -> Output {
        let mut full = vec!["submit"];
        full.extend_from_slice(args);
        let output = self.run(&full);
        assert!(
            output.status.success(),
            "stakk {full:?} failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            output.stdout,
            output.stderr,
        );
        output
    }

    /// `stakk graph --format=<format>` parsed as JSON.
    pub fn graph_json(&self, format: &str) -> serde_json::Value {
        let format_arg = format!("--format={format}");
        let args = ["graph", format_arg.as_str()];
        let output = self.run(&args);
        assert!(
            output.status.success(),
            "stakk {args:?} failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            output.stdout,
            output.stderr,
        );
        serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stakk {args:?} printed invalid JSON: {e}\n{}",
                output.stdout
            )
        })
    }
}

fn is_leaky(name: &OsString) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with("STAKK_")
        || name.starts_with("GH_")
        || name.starts_with("GITHUB_")
        || name == "FORGEJO_TOKEN"
}
