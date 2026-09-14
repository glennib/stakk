//! A thin Forgejo REST client for assertions. Deliberately independent of
//! `src/forge/forgejo/types.rs`: the structs carry only what the tests read.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Env;

#[derive(Debug, Deserialize)]
pub struct Repo {
    pub empty: bool,
    pub default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pull {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    pub head: PullRef,
    pub base: PullRef,
}

impl Pull {
    /// The body as text, `""` when Forgejo reports none.
    pub fn body_text(&self) -> &str {
        self.body.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRef {
    pub r#ref: String,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Branch {
    pub name: String,
    pub commit: BranchCommit,
}

#[derive(Debug, Deserialize)]
pub struct BranchCommit {
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct ForgejoApi {
    client: reqwest::Client,
    /// `<url>/api/v1`.
    api_base: String,
    owner: String,
    token: String,
}

impl ForgejoApi {
    pub fn new(env: &Env) -> Self {
        // reqwest is built with `rustls-no-provider`, so a process-level
        // provider has to be in place before a client is built, even for a
        // plain-http instance. An `Err` means one is installed already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            client,
            api_base: format!("{}/api/v1", env.url.trim_end_matches('/')),
            owner: env.user.clone(),
            token: env.token.clone(),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.api_base))
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> T {
        let response = self
            .request(reqwest::Method::GET, path)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert!(status.is_success(), "GET {path} -> {status}: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("GET {path}: {e}\n{text}"))
    }

    /// `GET` that maps a 404 to `None`.
    async fn get_optional<T: DeserializeOwned>(&self, path: &str) -> Option<T> {
        let response = self
            .request(reqwest::Method::GET, path)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return None;
        }
        let text = response.text().await.unwrap_or_default();
        assert!(status.is_success(), "GET {path} -> {status}: {text}");
        Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("GET {path}: {e}\n{text}")))
    }

    /// `POST /user/repos` with no auto-init, so the first push decides what
    /// `main` is.
    pub async fn create_repo(&self, name: &str) -> Repo {
        let path = "/user/repos";
        let response = self
            .request(reqwest::Method::POST, path)
            .json(&json!({
                "name": name,
                "auto_init": false,
                "default_branch": "main",
            }))
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path}: {e}"));
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "POST {path} -> {status}: {text}"
        );
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("POST {path}: {e}\n{text}"))
    }

    pub async fn get_repo(&self, repo: &str) -> Repo {
        self.get(&format!("/repos/{}/{repo}", self.owner)).await
    }

    /// `state` is `open`, `closed` or `all`. One page of 50, plenty for a
    /// test repository.
    pub async fn list_pulls(&self, repo: &str, state: &str) -> Vec<Pull> {
        self.get(&format!(
            "/repos/{}/{repo}/pulls?state={state}&page=1&limit=50",
            self.owner
        ))
        .await
    }

    pub async fn get_pull(&self, repo: &str, index: u64) -> Pull {
        self.get(&format!("/repos/{}/{repo}/pulls/{index}", self.owner))
            .await
    }

    pub async fn list_issue_comments(&self, repo: &str, index: u64) -> Vec<Comment> {
        self.get(&format!(
            "/repos/{}/{repo}/issues/{index}/comments?page=1&limit=50",
            self.owner
        ))
        .await
    }

    pub async fn list_branches(&self, repo: &str) -> Vec<Branch> {
        self.get(&format!(
            "/repos/{}/{repo}/branches?page=1&limit=50",
            self.owner
        ))
        .await
    }

    pub async fn get_branch(&self, repo: &str, name: &str) -> Option<Branch> {
        self.get_optional(&format!("/repos/{}/{repo}/branches/{name}", self.owner))
            .await
    }
}
