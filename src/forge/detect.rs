//! Which forge a remote host runs.
//!
//! `stakk submit` needs the forge before it can pick a token and a client,
//! and a remote URL alone does not always say: a self-hosted Forgejo and a
//! GitHub Enterprise Server look alike from one. [`classify`] answers from
//! what needs no network — an explicit `--forge`, the built-in public hosts,
//! and the user's host table ([`HostRules`]) — and reports every other host
//! as [`Classification::Unknown`]. For those, [`probe`] asks the host itself:
//! three unauthenticated `GET`s whose answers tell GitHub, Forgejo (Gitea)
//! and GitLab apart by tells verified against the real services, so a host
//! is recognised on first use and the user can pin it with `--host` or
//! `hosts` in `stakk.toml` to skip the round trip afterwards.
//!
//! `stakk graph` never asks: it is offline and reports hosts, not forges.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::header::LOCATION;
use miette::Diagnostic;
use thiserror::Error;

use crate::CODEBERG_ORG;
use crate::GITHUB_COM;
use crate::forge::ForgeKind;
use crate::forge::forgejo::transport::Transport;
use crate::forge::forgejo::transport::TransportError;
use crate::jj::remote::RemoteRepo;

/// Bitbucket Cloud. Recognised so the error can name it; stakk does not
/// submit to it.
const BITBUCKET_ORG: &str = "bitbucket.org";

/// gitlab.com, likewise.
const GITLAB_COM: &str = "gitlab.com";

/// One `HOST=FORGE` entry of the host table, as typed after `--host` or in
/// `STAKK_HOSTS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    /// The host as the remote URL names it, port included for an `http(s)`
    /// remote. Matched case-insensitively.
    pub host: String,
    pub forge: ForgeKind,
}

impl HostRule {
    /// clap value parser for `--host`: `HOST=FORGE`, split at the first `=`.
    /// The forge is spelled as `--forge` spells it, `github` or `forgejo`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let Some((host, forge)) = value.split_once('=') else {
            return Err(format!(
                "expected HOST=FORGE, e.g. git.example.com=forgejo, got '{value}'"
            ));
        };
        let host = host.trim();
        if host.is_empty() {
            return Err(format!(
                "expected HOST=FORGE with a non-empty host, got '{value}'"
            ));
        }
        let forge =
            <ForgeKind as clap::ValueEnum>::from_str(forge.trim(), false).map_err(|_| {
                format!("unknown forge '{forge}' in '{value}': expected github or forgejo")
            })?;
        Ok(Self {
            host: host.to_string(),
            forge,
        })
    }
}

impl std::fmt::Display for HostRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={}", self.host, self.forge)
    }
}

/// The host table with every layer folded in: host → forge, keys lowercased.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HostRules(BTreeMap<String, ForgeKind>);

impl HostRules {
    /// Fold the layers in, lowest precedence first: `GH_HOST` as a GitHub
    /// entry (the GitHub CLI's own setting, so an existing `gh` setup needs
    /// nothing further), then `hosts` from the config file, then the
    /// `--host`/`STAKK_HOSTS` list. A later layer overrides an earlier one
    /// for the same host, and within the list the last entry wins, as a
    /// repeated scalar flag would. An empty `GH_HOST` counts as unset.
    pub fn layered(
        gh_host: Option<&str>,
        config: Option<&BTreeMap<String, ForgeKind>>,
        cli: &[HostRule],
    ) -> Self {
        let mut rules = Self::default();
        if let Some(host) = gh_host.filter(|host| !host.is_empty()) {
            rules.insert(host, ForgeKind::Github);
        }
        for (host, forge) in config.into_iter().flatten() {
            rules.insert(host, *forge);
        }
        for rule in cli {
            rules.insert(&rule.host, rule.forge);
        }
        rules
    }

    fn insert(&mut self, host: &str, forge: ForgeKind) {
        self.0.insert(host.to_ascii_lowercase(), forge);
    }

    /// The forge `host` is mapped to, if any. Case-insensitive.
    pub fn get(&self, host: &str) -> Option<ForgeKind> {
        self.0.get(&host.to_ascii_lowercase()).copied()
    }
}

/// A forge stakk recognises but does not submit to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedForge {
    GitLab,
    BitbucketCloud,
}

impl UnsupportedForge {
    /// The forge's name as it appears in prose.
    pub fn label(self) -> &'static str {
        match self {
            Self::GitLab => "GitLab",
            Self::BitbucketCloud => "Bitbucket Cloud",
        }
    }
}

/// What [`classify`] can say about a host without a network round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Forge(ForgeKind),
    Unsupported(UnsupportedForge),
    /// Nothing offline says which forge this host runs.
    Unknown,
}

/// Decide the forge for `host` from what needs no network, in this order: an
/// explicit `--forge` (`STAKK_FORGE`, `forge` in `stakk.toml`) wins over
/// everything; then the built-in public hosts — github.com, codeberg.org, and
/// the recognised-but-unsupported bitbucket.org and gitlab.com — which no
/// rule can override; then the host table.
pub fn classify(host: &str, explicit: Option<ForgeKind>, rules: &HostRules) -> Classification {
    if let Some(kind) = explicit {
        return Classification::Forge(kind);
    }
    match host.to_ascii_lowercase().as_str() {
        GITHUB_COM => Classification::Forge(ForgeKind::Github),
        CODEBERG_ORG => Classification::Forge(ForgeKind::Forgejo),
        BITBUCKET_ORG => Classification::Unsupported(UnsupportedForge::BitbucketCloud),
        GITLAB_COM => Classification::Unsupported(UnsupportedForge::GitLab),
        host => rules
            .get(host)
            .map_or(Classification::Unknown, Classification::Forge),
    }
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

/// The forges the probe can tell apart, one request each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    Github,
    Forgejo,
    /// Recognised only to be refused by name.
    GitLab,
}

/// The probe requests for `repo`'s host, in [`Candidate`] order.
///
/// GitHub is always asked over `https`, as [`RemoteRepo::github_api_base_uri`]
/// is always `https`; Forgejo and GitLab over the remote URL's own scheme and
/// port, as [`RemoteRepo::forgejo_api_base_uri`] is, so a plain-`http`
/// instance on a port answers. The host never carries credentials:
/// `parse_remote_url` strips them.
pub fn probe_urls(repo: &RemoteRepo) -> [(Candidate, String); 3] {
    let host = &repo.host;
    let scheme = repo.scheme;
    [
        (Candidate::Github, format!("https://{host}/api/v3/meta")),
        (
            Candidate::Forgejo,
            format!("{scheme}://{host}/api/v1/version"),
        ),
        (
            Candidate::GitLab,
            format!("{scheme}://{host}/api/v4/version"),
        ),
    ]
}

/// One probe request and what came back, for the report a failed probe
/// prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAttempt {
    pub candidate: Candidate,
    pub url: String,
    /// The status line and content type, the redirect target, or the
    /// transport failure — whatever explains the verdict.
    pub summary: String,
    /// Whether the answer looked like `candidate`.
    pub positive: bool,
}

/// Every attempt of one probe, printed one `GET` per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub attempts: Vec<ProbeAttempt>,
}

impl std::fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, attempt) in self.attempts.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "  GET {} -> {}", attempt.url, attempt.summary)?;
        }
        Ok(())
    }
}

/// What the probe concluded about a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Detected(ForgeKind),
    Unsupported(UnsupportedForge),
    /// GitHub and Forgejo both answered positively — a proxy that answers
    /// everything, most likely.
    Ambiguous(ProbeReport),
    /// No supported forge answered positively.
    Unknown(ProbeReport),
}

/// One request's answer: a response of any status, or a failure below HTTP.
type Answer = Result<http::Response<String>, TransportError>;

/// Send the three probe requests concurrently and judge the answers.
///
/// Each request is an unauthenticated `GET` with `Accept: application/json`;
/// the transport adds the user agent and, for the real one
/// (`ReqwestTransport::probing`), refuses to follow redirects and bounds the
/// time each request may take.
pub async fn probe<T: Transport>(transport: &T, repo: &RemoteRepo) -> ProbeOutcome {
    let [github, forgejo, gitlab] = probe_urls(repo);
    let send = |url: &str| {
        let request = http::Request::get(url)
            .header(ACCEPT, "application/json")
            .body(String::new());
        async move {
            match request {
                Ok(request) => transport.send(request).await,
                // A host that does not form a valid URI cannot be probed;
                // reported like any other unreachable host.
                Err(e) => Err(TransportError::new(e)),
            }
        }
    };
    let (github_answer, forgejo_answer, gitlab_answer) =
        tokio::join!(send(&github.1), send(&forgejo.1), send(&gitlab.1));
    evaluate([
        (github.0, github.1, github_answer),
        (forgejo.0, forgejo.1, forgejo_answer),
        (gitlab.0, gitlab.1, gitlab_answer),
    ])
}

/// The pure verdict over the three answers.
///
/// Any `3xx` is negative for its candidate before anything else is looked
/// at. GitHub is an `X-GitHub-Request-Id` header on any other status — the
/// API stamps it on 401s and 404s alike, so a private-mode GitHub Enterprise
/// Server still identifies itself. Forgejo (or Gitea, same API) is a `200`
/// JSON body with a string `version`, or the `403` a sign-in-only instance
/// answers with, recognised by its fixed message. GitLab is an `X-Gitlab-Meta`
/// header. A transport failure is negative. One supported positive decides;
/// two are [`ProbeOutcome::Ambiguous`]; none is GitLab if that answered, else
/// [`ProbeOutcome::Unknown`].
pub fn evaluate(answers: [(Candidate, String, Answer); 3]) -> ProbeOutcome {
    let attempts: Vec<ProbeAttempt> = answers
        .into_iter()
        .map(|(candidate, url, answer)| {
            let (positive, summary) = match &answer {
                Ok(response) => (is_positive(candidate, response), summarize(response)),
                Err(e) => (false, e.to_string()),
            };
            ProbeAttempt {
                candidate,
                url,
                summary,
                positive,
            }
        })
        .collect();
    let positive = |candidate| {
        attempts
            .iter()
            .any(|a| a.candidate == candidate && a.positive)
    };
    let (github, forgejo, gitlab) = (
        positive(Candidate::Github),
        positive(Candidate::Forgejo),
        positive(Candidate::GitLab),
    );
    let report = ProbeReport { attempts };
    match (github, forgejo, gitlab) {
        (true, true, _) => ProbeOutcome::Ambiguous(report),
        (true, false, _) => ProbeOutcome::Detected(ForgeKind::Github),
        (false, true, _) => ProbeOutcome::Detected(ForgeKind::Forgejo),
        (false, false, true) => ProbeOutcome::Unsupported(UnsupportedForge::GitLab),
        (false, false, false) => ProbeOutcome::Unknown(report),
    }
}

/// The body Forgejo and Gitea answer every API call with under
/// `REQUIRE_SIGNIN_VIEW`, `/api/v1/version` included.
const FORGEJO_SIGN_IN_MESSAGE: &str = "Only signed in user is allowed to call APIs.";

fn is_positive(candidate: Candidate, response: &http::Response<String>) -> bool {
    if response.status().is_redirection() {
        return false;
    }
    match candidate {
        Candidate::Github => response.headers().contains_key("x-github-request-id"),
        Candidate::GitLab => response.headers().contains_key("x-gitlab-meta"),
        Candidate::Forgejo => is_forgejo_answer(response),
    }
}

fn is_forgejo_answer(response: &http::Response<String>) -> bool {
    let json = || serde_json::from_str::<serde_json::Value>(response.body()).ok();
    match response.status() {
        StatusCode::OK => {
            is_json(response)
                && json()
                    .is_some_and(|v| v.get("version").is_some_and(serde_json::Value::is_string))
        }
        StatusCode::FORBIDDEN => json().is_some_and(|v| {
            v.get("message").and_then(serde_json::Value::as_str) == Some(FORGEJO_SIGN_IN_MESSAGE)
        }),
        _ => false,
    }
}

fn content_type(response: &http::Response<String>) -> Option<&str> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim())
}

fn is_json(response: &http::Response<String>) -> bool {
    content_type(response) == Some("application/json")
}

/// `404 Not Found (text/html)`, or `302 Found -> https://…/users/sign_in`.
fn summarize(response: &http::Response<String>) -> String {
    let status = response.status();
    let mut summary = format!(
        "{} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    if status.is_redirection() {
        if let Some(location) = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
        {
            summary.push_str(" -> ");
            summary.push_str(location);
        }
    } else if let Some(content_type) = content_type(response) {
        let _ = write!(summary, " ({content_type})");
    }
    summary
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why `stakk submit` could not settle on a forge for the remote's host.
#[derive(Debug, Error, Diagnostic)]
pub enum DetectError {
    /// The host runs a forge stakk recognises and does not support.
    #[error(
        "remote host '{host}' runs {}, which stakk does not support",
        forge.label()
    )]
    #[diagnostic(
        code(stakk::detect::unsupported_forge),
        help("{}", unsupported_help(host))
    )]
    Unsupported {
        host: String,
        forge: UnsupportedForge,
    },

    /// The probe ran and no supported forge answered.
    #[error(
        "cannot tell which forge '{host}' runs; none of the probes answered like a supported \
         forge:\n{report}"
    )]
    #[diagnostic(
        code(stakk::detect::unknown_forge),
        help("{}", name_the_forge_help(host))
    )]
    Unknown { host: String, report: ProbeReport },

    /// The probe ran and both supported forges answered.
    #[error("'{host}' answered like both GitHub and Forgejo:\n{report}")]
    #[diagnostic(
        code(stakk::detect::ambiguous_forge),
        help("{}", name_the_forge_help(host))
    )]
    Ambiguous { host: String, report: ProbeReport },

    /// The probe's HTTP client could not be built, so nothing was asked.
    #[error("cannot build the HTTP client for the forge probe")]
    #[diagnostic(
        code(stakk::detect::client),
        help("skip the probe by naming the forge with --forge github|forgejo")
    )]
    Client(#[from] TransportError),
}

/// How to tell stakk the forge, for this run or once and for all — the advice
/// every detection failure ends with.
fn name_the_forge_help(host: &str) -> String {
    format!(
        "name the forge for this run with --forge github|forgejo (STAKK_FORGE, or forge in \
         stakk.toml), or map the host once with --host {host}=<forge> (STAKK_HOSTS) or hosts = {{ \
         \"{host}\" = \"<forge>\" }} in stakk.toml; a host behind a login page or a proxy usually \
         needs this"
    )
}

fn unsupported_help(host: &str) -> String {
    format!(
        "stakk submits to GitHub and Forgejo (Gitea) only; if {host} is one of those behind \
         another name, say so with --forge github|forgejo or --host {host}=<forge>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(host: &str, forge: ForgeKind) -> HostRule {
        HostRule {
            host: host.into(),
            forge,
        }
    }

    fn rules(cli: &[HostRule]) -> HostRules {
        HostRules::layered(None, None, cli)
    }

    // ---- classify ----

    #[test]
    fn explicit_beats_everything() {
        let rules = rules(&[rule("github.com", ForgeKind::Forgejo)]);
        assert_eq!(
            classify("github.com", Some(ForgeKind::Forgejo), &rules),
            Classification::Forge(ForgeKind::Forgejo)
        );
        assert_eq!(
            classify("gitlab.com", Some(ForgeKind::Github), &rules),
            Classification::Forge(ForgeKind::Github)
        );
    }

    #[test]
    fn builtin_public_hosts() {
        let none = HostRules::default();
        assert_eq!(
            classify("github.com", None, &none),
            Classification::Forge(ForgeKind::Github)
        );
        assert_eq!(
            classify("codeberg.org", None, &none),
            Classification::Forge(ForgeKind::Forgejo)
        );
        // Built-ins match case-insensitively like everything else.
        assert_eq!(
            classify("GitHub.com", None, &none),
            Classification::Forge(ForgeKind::Github)
        );
    }

    #[test]
    fn builtin_unsupported_hosts() {
        let none = HostRules::default();
        assert_eq!(
            classify("bitbucket.org", None, &none),
            Classification::Unsupported(UnsupportedForge::BitbucketCloud)
        );
        assert_eq!(
            classify("gitlab.com", None, &none),
            Classification::Unsupported(UnsupportedForge::GitLab)
        );
    }

    /// A rule cannot re-label a public host: github.com is GitHub whatever
    /// the table says, so a typo in `hosts` cannot send a GitHub token to
    /// the Forgejo client.
    #[test]
    fn builtin_beats_rules() {
        let rules = rules(&[
            rule("github.com", ForgeKind::Forgejo),
            rule("gitlab.com", ForgeKind::Github),
        ]);
        assert_eq!(
            classify("github.com", None, &rules),
            Classification::Forge(ForgeKind::Github)
        );
        assert_eq!(
            classify("gitlab.com", None, &rules),
            Classification::Unsupported(UnsupportedForge::GitLab)
        );
    }

    #[test]
    fn rules_match_case_insensitively() {
        let rules = rules(&[rule("GHE.Example.COM", ForgeKind::Github)]);
        assert_eq!(
            classify("ghe.example.com", None, &rules),
            Classification::Forge(ForgeKind::Github)
        );
        assert_eq!(rules.get("Ghe.Example.Com"), Some(ForgeKind::Github));
    }

    /// The port is part of the host, as `parse_remote_url` keeps it for
    /// http(s) remotes: `localhost:3000` and `localhost` are two hosts.
    #[test]
    fn rules_keep_the_port() {
        let rules = rules(&[rule("localhost:3000", ForgeKind::Forgejo)]);
        assert_eq!(
            classify("localhost:3000", None, &rules),
            Classification::Forge(ForgeKind::Forgejo)
        );
        assert_eq!(classify("localhost", None, &rules), Classification::Unknown);
    }

    #[test]
    fn unknown_host() {
        assert_eq!(
            classify("git.example.com", None, &HostRules::default()),
            Classification::Unknown
        );
    }

    // ---- HostRules::layered ----

    #[test]
    fn layered_cli_beats_config_beats_gh_host() {
        let config = BTreeMap::from([
            ("shared.example.com".to_string(), ForgeKind::Forgejo),
            ("config-only.example.com".to_string(), ForgeKind::Forgejo),
            ("ghe.example.com".to_string(), ForgeKind::Forgejo),
        ]);
        let cli = [rule("shared.example.com", ForgeKind::Github)];
        let rules = HostRules::layered(Some("ghe.example.com"), Some(&config), &cli);
        // CLI over config.
        assert_eq!(rules.get("shared.example.com"), Some(ForgeKind::Github));
        // Config over GH_HOST.
        assert_eq!(rules.get("ghe.example.com"), Some(ForgeKind::Forgejo));
        // Lower layers still fill the gaps.
        assert_eq!(
            rules.get("config-only.example.com"),
            Some(ForgeKind::Forgejo)
        );
    }

    #[test]
    fn gh_host_alone_is_a_github_entry() {
        let rules = HostRules::layered(Some("ghe.example.com"), None, &[]);
        assert_eq!(rules.get("ghe.example.com"), Some(ForgeKind::Github));
    }

    #[test]
    fn gh_host_empty_is_ignored() {
        assert_eq!(
            HostRules::layered(Some(""), None, &[]),
            HostRules::default()
        );
    }

    #[test]
    fn duplicate_cli_host_last_wins() {
        let rules = rules(&[
            rule("git.example.com", ForgeKind::Github),
            rule("git.example.com", ForgeKind::Forgejo),
        ]);
        assert_eq!(rules.get("git.example.com"), Some(ForgeKind::Forgejo));
    }

    // ---- HostRule::parse ----

    #[test]
    fn host_rule_parse_ok() {
        assert_eq!(
            HostRule::parse("git.example.com=forgejo"),
            Ok(rule("git.example.com", ForgeKind::Forgejo))
        );
        assert_eq!(
            HostRule::parse("localhost:3000=forgejo"),
            Ok(rule("localhost:3000", ForgeKind::Forgejo))
        );
        // Whitespace around either side is tolerated, as in a hand-typed env
        // var.
        assert_eq!(
            HostRule::parse(" ghe.example.com = github "),
            Ok(rule("ghe.example.com", ForgeKind::Github))
        );
    }

    #[test]
    fn host_rule_parse_missing_eq() {
        let err = HostRule::parse("git.example.com").unwrap_err();
        assert!(err.contains("HOST=FORGE"), "{err}");
    }

    #[test]
    fn host_rule_parse_empty_host() {
        let err = HostRule::parse("=forgejo").unwrap_err();
        assert!(err.contains("non-empty host"), "{err}");
    }

    #[test]
    fn host_rule_parse_unknown_forge() {
        let err = HostRule::parse("git.example.com=gitlab").unwrap_err();
        assert!(err.contains("unknown forge 'gitlab'"), "{err}");
        assert!(err.contains("github or forgejo"), "{err}");
        // The forge is spelled as `--forge` spells it: no case folding.
        assert!(HostRule::parse("git.example.com=Forgejo").is_err());
    }

    #[test]
    fn host_rule_display_round_trips() {
        let rule = rule("git.example.com", ForgeKind::Forgejo);
        assert_eq!(rule.to_string(), "git.example.com=forgejo");
        assert_eq!(HostRule::parse(&rule.to_string()), Ok(rule));
    }

    // ---- DetectError ----

    fn help(err: &DetectError) -> String {
        miette::Diagnostic::help(err).unwrap().to_string()
    }

    fn empty_report() -> ProbeReport {
        ProbeReport {
            attempts: Vec::new(),
        }
    }

    #[test]
    fn unknown_help_names_flag_env_and_config_key() {
        let err = DetectError::Unknown {
            host: "git.example.com".into(),
            report: empty_report(),
        };
        assert!(
            err.to_string()
                .starts_with("cannot tell which forge 'git.example.com' runs"),
            "{err}"
        );
        let help = help(&err);
        assert!(help.contains("--forge github|forgejo"), "{help}");
        assert!(help.contains("STAKK_FORGE"), "{help}");
        assert!(help.contains("--host git.example.com=<forge>"), "{help}");
        assert!(help.contains("STAKK_HOSTS"), "{help}");
        assert!(
            help.contains(r#"hosts = { "git.example.com" = "<forge>" }"#),
            "{help}"
        );
    }

    #[test]
    fn unsupported_names_the_forge() {
        let err = DetectError::Unsupported {
            host: "gitlab.com".into(),
            forge: UnsupportedForge::GitLab,
        };
        assert_eq!(
            err.to_string(),
            "remote host 'gitlab.com' runs GitLab, which stakk does not support"
        );
        let help = help(&err);
        assert!(help.contains("GitHub and Forgejo (Gitea) only"), "{help}");
        assert!(help.contains("--host gitlab.com=<forge>"), "{help}");

        let err = DetectError::Unsupported {
            host: "bitbucket.org".into(),
            forge: UnsupportedForge::BitbucketCloud,
        };
        assert!(err.to_string().contains("runs Bitbucket Cloud"));
    }

    #[test]
    fn ambiguous_message_lists_every_attempt() {
        let report = evaluate([
            (
                Candidate::Github,
                "https://x.example.com/api/v3/meta".into(),
                Ok(with_header(json(200, "{}"), "x-github-request-id", "abc")),
            ),
            (
                Candidate::Forgejo,
                "https://x.example.com/api/v1/version".into(),
                Ok(json(200, r#"{"version":"1.0"}"#)),
            ),
            (
                Candidate::GitLab,
                "https://x.example.com/api/v4/version".into(),
                Ok(html(404)),
            ),
        ]);
        let ProbeOutcome::Ambiguous(report) = report else {
            panic!("expected Ambiguous, got {report:?}");
        };
        let err = DetectError::Ambiguous {
            host: "x.example.com".into(),
            report,
        };
        let message = err.to_string();
        assert!(
            message.starts_with("'x.example.com' answered like both GitHub and Forgejo:"),
            "{message}"
        );
        assert!(
            message
                .contains("  GET https://x.example.com/api/v3/meta -> 200 OK (application/json)"),
            "{message}"
        );
        assert!(
            message.contains(
                "  GET https://x.example.com/api/v4/version -> 404 Not Found (text/html)"
            ),
            "{message}"
        );
        assert!(help(&err).contains("--host x.example.com=<forge>"));
    }

    // ---- the probe: URLs ----

    fn remote(url: &str) -> RemoteRepo {
        crate::jj::remote::parse_remote_url(url).unwrap()
    }

    /// Forgejo and GitLab are asked over the remote's own scheme and port,
    /// GitHub always over https, and credentials never travel.
    #[test]
    fn probe_urls_follow_the_api_base_rules() {
        let urls = probe_urls(&remote("http://user:token@localhost:3000/o/r.git"));
        assert_eq!(
            urls,
            [
                (
                    Candidate::Github,
                    "https://localhost:3000/api/v3/meta".to_string()
                ),
                (
                    Candidate::Forgejo,
                    "http://localhost:3000/api/v1/version".to_string()
                ),
                (
                    Candidate::GitLab,
                    "http://localhost:3000/api/v4/version".to_string()
                ),
            ]
        );
    }

    #[test]
    fn probe_urls_turn_ssh_into_https() {
        let urls = probe_urls(&remote("ssh://git@git.example.com:2222/o/r.git"));
        assert_eq!(urls[1].1, "https://git.example.com/api/v1/version");
        assert_eq!(urls[2].1, "https://git.example.com/api/v4/version");
    }

    // ---- the probe: verdicts ----

    use crate::forge::forgejo::transport::testing::Answer as Routed;
    use crate::forge::forgejo::transport::testing::RoutedTransport;
    use crate::forge::forgejo::transport::testing::response;

    fn with_header(
        mut response: http::Response<String>,
        name: &'static str,
        value: &str,
    ) -> http::Response<String> {
        response.headers_mut().insert(name, value.parse().unwrap());
        response
    }

    fn json(status: u16, body: &str) -> http::Response<String> {
        with_header(
            response(status, body),
            "content-type",
            "application/json;charset=utf-8",
        )
    }

    fn html(status: u16) -> http::Response<String> {
        with_header(
            response(status, "<!doctype html><html></html>"),
            "content-type",
            "text/html; charset=utf-8",
        )
    }

    fn redirect(location: &str) -> http::Response<String> {
        with_header(response(302, ""), "location", location)
    }

    fn refused() -> Answer {
        Err(TransportError::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        )))
    }

    fn github_header(response: http::Response<String>) -> http::Response<String> {
        with_header(response, "x-github-request-id", "C770:348DD9:C761D:AD8A2")
    }

    fn gitlab_header(response: http::Response<String>) -> http::Response<String> {
        with_header(
            response,
            "x-gitlab-meta",
            r#"{"correlation_id":"x","version":"1"}"#,
        )
    }

    /// The verdict for one answer per candidate; the URLs are immaterial.
    fn verdict(github: Answer, forgejo: Answer, gitlab: Answer) -> ProbeOutcome {
        evaluate([
            (Candidate::Github, "https://h/api/v3/meta".into(), github),
            (
                Candidate::Forgejo,
                "https://h/api/v1/version".into(),
                forgejo,
            ),
            (Candidate::GitLab, "https://h/api/v4/version".into(), gitlab),
        ])
    }

    fn report_of(outcome: ProbeOutcome) -> ProbeReport {
        match outcome {
            ProbeOutcome::Unknown(report) | ProbeOutcome::Ambiguous(report) => report,
            other => panic!("expected a report, got {other:?}"),
        }
    }

    const GITHUB_404: &str = r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest","status":"404"}"#;
    const FORGEJO_VERSION: &str = r#"{"version":"11.0.1+gitea-1.22.0"}"#;
    const GITEA_VERSION: &str = r#"{"version":"1.24.0"}"#;
    const FORGEJO_SIGN_IN: &str = r#"{"message":"Only signed in user is allowed to call APIs.","url":"http://localhost:3000/api/swagger"}"#;

    /// github.com itself answers 404 on `/api/v3/meta` (its API lives on
    /// api.github.com), header included — the header, not the status, is
    /// the tell.
    #[test]
    fn github_is_the_request_id_header_on_any_status() {
        assert_eq!(
            verdict(
                Ok(github_header(json(404, GITHUB_404))),
                Ok(html(404)),
                Ok(html(404))
            ),
            ProbeOutcome::Detected(ForgeKind::Github)
        );
        // A GHES behind single sign-on: 401 from the API, logins elsewhere.
        assert_eq!(
            verdict(
                Ok(github_header(json(
                    401,
                    r#"{"message":"Requires authentication"}"#
                ))),
                Ok(redirect("https://ghe.example.com/login")),
                Ok(redirect("https://ghe.example.com/login")),
            ),
            ProbeOutcome::Detected(ForgeKind::Github)
        );
    }

    #[test]
    fn header_names_match_case_insensitively() {
        let lower = with_header(json(404, GITHUB_404), "x-github-request-id", "abc");
        assert_eq!(
            verdict(Ok(lower), Ok(html(404)), Ok(html(404))),
            ProbeOutcome::Detected(ForgeKind::Github)
        );
    }

    #[test]
    fn forgejo_is_a_json_version_object() {
        assert_eq!(
            verdict(
                Ok(response(404, "Not found.")),
                Ok(json(200, FORGEJO_VERSION)),
                Ok(html(404))
            ),
            ProbeOutcome::Detected(ForgeKind::Forgejo)
        );
        // Gitea speaks the same API and counts as Forgejo.
        assert_eq!(
            verdict(
                Ok(response(404, "Not found.")),
                Ok(json(200, GITEA_VERSION)),
                Ok(html(404))
            ),
            ProbeOutcome::Detected(ForgeKind::Forgejo)
        );
    }

    #[test]
    fn forgejo_requiring_sign_in_is_its_fixed_403_message() {
        assert_eq!(
            verdict(
                Ok(response(404, "")),
                Ok(json(403, FORGEJO_SIGN_IN)),
                Ok(html(404))
            ),
            ProbeOutcome::Detected(ForgeKind::Forgejo)
        );
        // Any other 403 is not a tell.
        assert!(matches!(
            verdict(
                Ok(response(404, "")),
                Ok(json(403, r#"{"message":"forbidden"}"#)),
                Ok(html(404))
            ),
            ProbeOutcome::Unknown(_)
        ));
    }

    /// A version key that is not a string, a JSON array, or JSON served as
    /// HTML: none of them is Forgejo.
    #[test]
    fn forgejo_needs_the_exact_shape() {
        for body in [r#"{"version":1}"#, "[]", r#"{"ver":"1"}"#] {
            assert!(
                matches!(
                    verdict(Ok(response(404, "")), Ok(json(200, body)), Ok(html(404))),
                    ProbeOutcome::Unknown(_)
                ),
                "{body}"
            );
        }
        let html_typed = with_header(response(200, FORGEJO_VERSION), "content-type", "text/html");
        assert!(matches!(
            verdict(Ok(response(404, "")), Ok(html_typed), Ok(html(404))),
            ProbeOutcome::Unknown(_)
        ));
    }

    /// A single-page app that answers 200 HTML for every path (tangled.org
    /// does this for some) is nothing stakk knows.
    #[test]
    fn spa_fallback_is_unknown() {
        let outcome = verdict(Ok(html(200)), Ok(html(200)), Ok(html(200)));
        let report = report_of(outcome);
        assert!(report.attempts.iter().all(|a| !a.positive));
        assert!(
            report.attempts[0].summary.contains("200 OK (text/html)"),
            "{}",
            report.attempts[0].summary
        );
    }

    #[test]
    fn gitlab_is_the_meta_header_and_unsupported() {
        assert_eq!(
            verdict(
                Ok(redirect("https://gitlab.example.com/users/sign_in")),
                Ok(redirect("https://gitlab.example.com/users/sign_in")),
                Ok(gitlab_header(json(
                    401,
                    r#"{"message":"401 Unauthorized"}"#
                ))),
            ),
            ProbeOutcome::Unsupported(UnsupportedForge::GitLab)
        );
    }

    /// A redirect is negative for its candidate before any header is read:
    /// a login page in front of everything says nothing about the forge.
    #[test]
    fn a_redirect_is_never_positive() {
        let outcome = verdict(
            Ok(github_header(with_header(
                response(301, ""),
                "location",
                "https://sso/",
            ))),
            Ok(redirect("https://sso/")),
            Ok(redirect("https://sso/")),
        );
        let report = report_of(outcome);
        assert!(report.attempts.iter().all(|a| !a.positive));
        assert_eq!(
            report.attempts[0].summary,
            "301 Moved Permanently -> https://sso/"
        );
        assert_eq!(report.attempts[1].summary, "302 Found -> https://sso/");
    }

    #[test]
    fn transport_failures_are_negative_and_reported() {
        let report = report_of(verdict(refused(), refused(), refused()));
        assert!(report.attempts.iter().all(|a| !a.positive));
        for attempt in &report.attempts {
            assert!(
                attempt.summary.contains("connection refused"),
                "{}",
                attempt.summary
            );
        }
    }

    #[test]
    fn both_supported_positive_is_ambiguous() {
        assert!(matches!(
            verdict(
                Ok(github_header(json(200, "{}"))),
                Ok(json(200, FORGEJO_VERSION)),
                Ok(html(404))
            ),
            ProbeOutcome::Ambiguous(_)
        ));
    }

    /// GitLab positive next to a supported positive does not veto it.
    #[test]
    fn a_supported_answer_beats_gitlab() {
        assert_eq!(
            verdict(
                Ok(github_header(json(200, "{}"))),
                Ok(html(404)),
                Ok(gitlab_header(json(401, "{}"))),
            ),
            ProbeOutcome::Detected(ForgeKind::Github)
        );
    }

    // ---- the probe: requests ----

    #[tokio::test]
    async fn probe_sends_three_unauthenticated_gets() {
        let repo = remote("http://user:token@localhost:3000/o/r.git");
        let transport = RoutedTransport::new([
            (
                "https://localhost:3000/api/v3/meta".to_string(),
                Routed::Refused,
            ),
            (
                "http://localhost:3000/api/v1/version".to_string(),
                Routed::Response(json(200, FORGEJO_VERSION)),
            ),
            (
                "http://localhost:3000/api/v4/version".to_string(),
                Routed::Response(html(404)),
            ),
        ]);
        assert_eq!(
            probe(&transport, &repo).await,
            ProbeOutcome::Detected(ForgeKind::Forgejo)
        );
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter() {
            assert_eq!(request.method(), http::Method::GET);
            assert_eq!(request.headers().get(ACCEPT).unwrap(), "application/json");
            assert!(request.headers().get(http::header::AUTHORIZATION).is_none());
            assert!(request.body().is_empty());
            assert!(!request.uri().to_string().contains("token"));
        }
    }
}
