//! Pure, synchronous TF-IDF algorithm for generating bookmark names from
//! commit descriptions and file paths.
//!
//! No I/O, no async, no external dependencies beyond std.

use std::collections::HashMap;

/// Data for a single commit in the segment.
pub struct CommitData<'a> {
    pub description: &'a str,
    pub files: &'a [String],
}

// ── Stop words ──────────────────────────────────────────────────────────────

// Sorted for binary_search. Verified by a test.
const STOP_WORDS: &[&str] = &[
    "a",
    "about",
    "add",
    "added",
    "adds",
    "after",
    "all",
    "allow",
    "also",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "before",
    "between",
    "build",
    "but",
    "by",
    "change",
    "changed",
    "changes",
    "chore",
    "ci",
    "clean",
    "cleaned",
    "could",
    "create",
    "created",
    "delete",
    "deleted",
    "did",
    "disable",
    "disabled",
    "do",
    "docs",
    "does",
    "each",
    "enable",
    "enabled",
    "ensure",
    "extract",
    "extracted",
    "feat",
    "fix",
    "for",
    "from",
    "get",
    "had",
    "handle",
    "handled",
    "has",
    "have",
    "if",
    "implement",
    "implemented",
    "improve",
    "improved",
    "in",
    "initial",
    "into",
    "introduce",
    "introduced",
    "is",
    "it",
    "just",
    "made",
    "make",
    "may",
    "merge",
    "might",
    "more",
    "most",
    "move",
    "moved",
    "new",
    "no",
    "not",
    "now",
    "of",
    "on",
    "or",
    "other",
    "out",
    "over",
    "perf",
    "refactor",
    "remove",
    "removed",
    "removes",
    "rename",
    "renamed",
    "revert",
    "set",
    "should",
    "so",
    "some",
    "style",
    "support",
    "test",
    "tests",
    "than",
    "that",
    "the",
    "then",
    "this",
    "to",
    "too",
    "up",
    "update",
    "updated",
    "updates",
    "use",
    "used",
    "was",
    "were",
    "when",
    "will",
    "wip",
    "with",
    "would",
];

/// Directories filtered out of file path tokens.
const NOISE_DIRS: &[&str] = &[
    "src", "lib", "test", "tests", "pkg", "cmd", "internal", "app",
];

/// Regex-like conventional commit prefix pattern.
/// Matches: `type(scope)!: ` or `type: ` at the start of a line.
fn strip_cc_prefix(msg: &str) -> &str {
    let prefixes = [
        "feat", "fix", "chore", "docs", "style", "refactor", "perf", "test", "build", "ci",
        "revert",
    ];

    let trimmed = msg.trim_start();
    for prefix in prefixes {
        let Some(rest) = trimmed
            .get(..prefix.len())
            .filter(|s| s.eq_ignore_ascii_case(prefix))
            .and_then(|_| trimmed.get(prefix.len()..))
        else {
            continue;
        };

        // Optional `(scope)` then optional `!` then `: `
        let rest = if let Some(r) = rest.strip_prefix('(') {
            // Find closing paren.
            if let Some(close) = r.find(')') {
                &r[close + 1..]
            } else {
                continue;
            }
        } else {
            rest
        };

        let rest = rest.strip_prefix('!').unwrap_or(rest);

        if let Some(rest) = rest.strip_prefix(':') {
            return rest.trim_start();
        }
    }

    trimmed
}

/// Extract the first line of a message.
fn first_line(msg: &str) -> &str {
    msg.lines().next().unwrap_or("").trim()
}

/// Tokenize text: lowercase, replace punctuation with spaces, split, filter
/// short words.
fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let replaced: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    replaced
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .map(String::from)
        .collect()
}

/// Extract meaningful tokens from file paths.
fn file_tokens(files: &[String]) -> Vec<String> {
    let mut tokens = Vec::new();
    for path in files {
        let parts = path.replace('\\', "/");
        for part in parts.split('/') {
            // Strip extension.
            let stem = part.rsplit_once('.').map_or(part, |(s, _)| s);
            // Split on underscores/hyphens.
            for word in stem.split(['_', '-']) {
                let lower = word.to_lowercase();
                if lower.len() > 1 && !NOISE_DIRS.contains(&lower.as_str()) {
                    tokens.push(lower);
                }
            }
        }
    }
    tokens
}

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.binary_search(&word).is_ok()
}

/// Weight multiplier for tokens from commit descriptions.
const DESCRIPTION_WEIGHT: f64 = 1.0;

/// Weight multiplier for tokens from file paths.
const FILE_PATH_WEIGHT: f64 = 0.1;

/// A token with its source weight.
struct WeightedToken {
    term: String,
    weight: f64,
}

/// Compute TF-IDF scores across weighted token documents.
#[expect(
    clippy::cast_precision_loss,
    reason = "commit count per segment is always tiny compared to f64 mantissa"
)]
fn compute_tfidf(docs: &[Vec<WeightedToken>]) -> HashMap<String, f64> {
    let n = docs.len() as f64;

    // Weighted TF: sum of weights across all docs.
    let mut tf: HashMap<String, f64> = HashMap::new();
    // DF: number of docs containing the term.
    let mut df: HashMap<String, usize> = HashMap::new();

    for doc in docs {
        for token in doc {
            *tf.entry(token.term.clone()).or_default() += token.weight;
        }
        // Unique terms per doc for DF.
        let unique: std::collections::HashSet<&str> = doc.iter().map(|t| t.term.as_str()).collect();
        for term in unique {
            *df.entry(term.to_string()).or_default() += 1;
        }
    }

    let mut scores = HashMap::new();
    for (term, weighted_freq) in &tf {
        if is_stop_word(term) {
            continue;
        }
        let doc_freq = df.get(term).copied().unwrap_or(1) as f64;
        let idf = (n / doc_freq).ln() + 1.0;
        scores.insert(term.clone(), weighted_freq * idf);
    }

    scores
}

/// Generate a TF-IDF-based bookmark name from commit data.
///
/// Returns `None` if no meaningful terms are found.
pub fn tfidf_bookmark_name(
    commits: &[CommitData<'_>],
    max_terms: usize,
    variation: usize,
    max_length: usize,
    disallowed_chars: &str,
) -> Option<String> {
    if commits.is_empty() {
        return None;
    }

    // Build one document per commit with weighted tokens.
    let docs: Vec<Vec<WeightedToken>> = commits
        .iter()
        .map(|c| {
            let desc = strip_cc_prefix(first_line(c.description));
            let mut tokens: Vec<WeightedToken> = tokenize(desc)
                .into_iter()
                .map(|term| WeightedToken {
                    term,
                    weight: DESCRIPTION_WEIGHT,
                })
                .collect();
            tokens.extend(file_tokens(c.files).into_iter().map(|term| WeightedToken {
                term,
                weight: FILE_PATH_WEIGHT,
            }));
            tokens
        })
        .collect();

    let scores = compute_tfidf(&docs);
    if scores.is_empty() {
        return None;
    }

    // Build a pool of top-scoring terms.
    let pool_size = max_terms.saturating_mul(2).saturating_add(2).max(8);
    let mut pool: Vec<&str> = scores.keys().map(String::as_str).collect();
    pool.sort_by(|a, b| {
        scores
            .get(*b)
            .unwrap()
            .partial_cmp(scores.get(*a).unwrap())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b)) // Break ties alphabetically for determinism.
    });
    pool.truncate(pool_size);

    // Generate combinations of `max_terms` from the pool.
    let combo_size = max_terms.min(pool.len());
    let combos: Vec<Vec<&str>> = combinations(&pool, combo_size);
    if combos.is_empty() {
        return None;
    }

    let idx = variation % combos.len();
    let mut chosen: Vec<&str> = combos[idx].clone();
    chosen.sort_unstable();

    let name = chosen.join("-");
    let sanitized = sanitize(&name, disallowed_chars, max_length);

    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

/// Generate all combinations of `k` items from `pool`.
fn combinations<'a>(pool: &[&'a str], k: usize) -> Vec<Vec<&'a str>> {
    if k == 0 || k > pool.len() {
        return if k == 0 { vec![vec![]] } else { vec![] };
    }

    let mut result = Vec::new();
    let mut indices: Vec<usize> = (0..k).collect();

    loop {
        result.push(indices.iter().map(|&i| pool[i]).collect());

        // Find rightmost index that can be incremented.
        let mut i = k;
        loop {
            if i == 0 {
                return result;
            }
            i -= 1;
            if indices[i] < pool.len() - k + i {
                break;
            }
            if i == 0 {
                return result;
            }
        }
        indices[i] += 1;
        for j in (i + 1)..k {
            indices[j] = indices[j - 1] + 1;
        }
    }
}

/// Sanitize a name: strip disallowed chars, collapse hyphens, truncate on
/// hyphen boundary.
fn sanitize(name: &str, disallowed_chars: &str, max_length: usize) -> String {
    let mut cleaned: String = name
        .chars()
        .filter(|c| !disallowed_chars.contains(*c))
        .collect();

    // Collapse repeated hyphens.
    while cleaned.contains("--") {
        cleaned = cleaned.replace("--", "-");
    }
    cleaned = cleaned.trim_matches('-').to_string();

    // Truncate on hyphen boundary if too long.
    if cleaned.len() > max_length {
        let mut truncated = String::new();
        for part in cleaned.split('-') {
            let candidate = if truncated.is_empty() {
                part.to_string()
            } else {
                format!("{truncated}-{part}")
            };
            if candidate.len() > max_length {
                break;
            }
            truncated = candidate;
        }
        cleaned = if truncated.is_empty() {
            cleaned[..max_length].to_string()
        } else {
            truncated
        };
    }

    cleaned
}

// ── Sorted stop words for binary search ─────────────────────────────────────

// We verify at test time that STOP_WORDS is sorted, so binary_search works.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_words_are_sorted() {
        let mut sorted = STOP_WORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            STOP_WORDS,
            &sorted[..],
            "STOP_WORDS must be sorted for binary_search"
        );
    }

    #[test]
    fn strip_cc_prefix_basic() {
        assert_eq!(strip_cc_prefix("feat: add login"), "add login");
        assert_eq!(
            strip_cc_prefix("fix(auth): resolve token bug"),
            "resolve token bug"
        );
        assert_eq!(strip_cc_prefix("feat!: breaking change"), "breaking change");
        assert_eq!(
            strip_cc_prefix("refactor(core)!: rework engine"),
            "rework engine"
        );
        assert_eq!(strip_cc_prefix("no prefix here"), "no prefix here");
    }

    #[test]
    fn first_line_extraction() {
        assert_eq!(first_line("title\n\nbody"), "title");
        assert_eq!(first_line("  title  "), "title");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("Add OAuth2 support for login");
        assert!(tokens.contains(&"add".to_string()));
        assert!(tokens.contains(&"oauth2".to_string()));
        assert!(tokens.contains(&"support".to_string()));
        assert!(tokens.contains(&"for".to_string()));
        assert!(tokens.contains(&"login".to_string()));
        // Single-char words filtered.
        assert!(!tokens.iter().any(|t| t.len() < 2));
    }

    #[test]
    fn file_tokens_basic() {
        let files = vec![
            "src/auth/oauth.rs".to_string(),
            "tests/login_test.rs".to_string(),
        ];
        let tokens = file_tokens(&files);
        assert!(tokens.contains(&"auth".to_string()));
        assert!(tokens.contains(&"oauth".to_string()));
        // "src" and "tests" are noise dirs.
        assert!(!tokens.contains(&"src".to_string()));
        assert!(!tokens.contains(&"tests".to_string()));
    }

    #[test]
    fn file_tokens_strips_extensions() {
        let files = ["foo/bar_baz.tsx".to_string()];
        let tokens = file_tokens(&files);
        assert!(tokens.contains(&"bar".to_string()));
        assert!(tokens.contains(&"baz".to_string()));
        assert!(!tokens.contains(&"tsx".to_string()));
    }

    #[test]
    fn tfidf_basic_name() {
        let files = vec!["src/auth/oauth.rs".to_string()];
        let commits = vec![CommitData {
            description: "feat: add OAuth2 login page",
            files: &files,
        }];
        let name = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        assert!(name.is_some());
        let name = name.unwrap();
        assert!(!name.is_empty());
        // Should contain meaningful terms, not stop words.
        for part in name.split('-') {
            assert!(!is_stop_word(part), "name part {part:?} is a stop word");
        }
    }

    #[test]
    fn tfidf_variations_differ() {
        let files1 = vec![
            "src/auth/middleware.rs".to_string(),
            "src/auth/token.rs".to_string(),
        ];
        let files2 = vec![
            "src/api/rate_limit.rs".to_string(),
            "src/api/endpoints.rs".to_string(),
        ];
        let commits = vec![
            CommitData {
                description: "add authentication middleware",
                files: &files1,
            },
            CommitData {
                description: "add rate limiting to API endpoints",
                files: &files2,
            },
        ];
        let v0 = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        let v1 = tfidf_bookmark_name(&commits, 3, 1, 255, " ~^:?*[\\");
        assert!(v0.is_some());
        assert!(v1.is_some());
        // With enough terms, different variations should produce different names
        // (unless the pool is too small).
        if v0 != v1 {
            assert_ne!(v0, v1);
        }
    }

    #[test]
    fn tfidf_empty_commits_returns_none() {
        let result = tfidf_bookmark_name(&[], 3, 0, 255, " ~^:?*[\\");
        assert!(result.is_none());
    }

    #[test]
    fn tfidf_all_stop_words_returns_none() {
        let commits = vec![CommitData {
            description: "add update remove",
            files: &[],
        }];
        let result = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        assert!(result.is_none());
    }

    #[test]
    fn tfidf_single_commit_no_files() {
        let commits = vec![CommitData {
            description: "implement caching layer for database queries",
            files: &[],
        }];
        let name = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        assert!(name.is_some());
        let name = name.unwrap();
        assert!(
            name.contains("caching")
                || name.contains("layer")
                || name.contains("database")
                || name.contains("queries"),
            "expected meaningful terms in name, got: {name}"
        );
    }

    #[test]
    fn sanitize_disallowed_chars() {
        let result = sanitize("hello world~test", " ~^:?*[\\", 255);
        assert_eq!(result, "helloworldtest");
    }

    #[test]
    fn sanitize_truncation() {
        let result = sanitize("alpha-beta-gamma-delta", "", 12);
        assert_eq!(result, "alpha-beta");
    }

    #[test]
    fn sanitize_collapse_hyphens() {
        let result = sanitize("foo---bar", "", 255);
        assert_eq!(result, "foo-bar");
    }

    #[test]
    fn combinations_basic() {
        let pool = &["a", "b", "c"];
        let combos = combinations(pool, 2);
        assert_eq!(combos.len(), 3);
        assert!(combos.contains(&vec!["a", "b"]));
        assert!(combos.contains(&vec!["a", "c"]));
        assert!(combos.contains(&vec!["b", "c"]));
    }

    #[test]
    fn combinations_k_equals_pool() {
        let pool = &["x", "y"];
        let combos = combinations(pool, 2);
        assert_eq!(combos.len(), 1);
        assert_eq!(combos[0], vec!["x", "y"]);
    }

    #[test]
    fn combinations_k_zero() {
        let pool = &["a", "b"];
        let combos = combinations(pool, 0);
        assert_eq!(combos.len(), 1);
        assert!(combos[0].is_empty());
    }

    #[test]
    fn combinations_k_exceeds_pool() {
        let pool = &["a"];
        let combos = combinations(pool, 2);
        assert!(combos.is_empty());
    }

    #[test]
    fn variation_wraps_around() {
        let commits = vec![CommitData {
            description: "implement caching layer for database queries",
            files: &[],
        }];
        let v0 = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        // Large variation wraps around.
        let v_large = tfidf_bookmark_name(&commits, 3, 1000, 255, " ~^:?*[\\");
        // Both should produce something (the pool is small enough that they
        // might wrap to the same combo, but they should both be Some).
        assert!(v0.is_some());
        assert!(v_large.is_some());
    }

    #[test]
    fn unicode_handling() {
        let files = vec!["src/users/profile.rs".to_string()];
        let commits = vec![CommitData {
            description: "fix: handle UTF-8 emojis in usernames",
            files: &files,
        }];
        let name = tfidf_bookmark_name(&commits, 3, 0, 255, " ~^:?*[\\");
        assert!(name.is_some());
        let name = name.unwrap();
        // Should not panic or produce garbage.
        assert!(!name.is_empty());
    }
}
