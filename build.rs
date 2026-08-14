//! Turn the `docs/` directory into the `stakk docs` topic set.
//!
//! Every Markdown file there is a topic. This reads the directory, parses the
//! metadata preamble each file carries, and generates two things into
//! `OUT_DIR`: the `DocTopic` enum (one variant per file, documented with the
//! file's `summary`) and the `source` function that maps a topic to its text.
//! `src/docs` includes the result, so adding a document is one edit — the file
//! itself — and a file that cannot be turned into a topic fails the build here
//! rather than shipping unreachable.
//!
//! The preamble is an HTML comment, so GitHub drops it when rendering the file
//! and `rumdl` leaves it alone:
//!
//! ```text
//! <!--- stakk-docs
//! summary: Config files, precedence, and environment variables.
//! --->
//!
//! # Configuration
//! ```
//!
//! It is `key: value` lines and nothing more — not YAML, despite the shape. The
//! value is the rest of the line, verbatim, so a summary may contain a colon
//! and needs no quoting. `summary` is the only key; anything else is an error,
//! which is what keeps the format from growing by accident.
//!
//! The body written to `OUT_DIR` is the file below its preamble. That is what
//! `stakk docs <topic>` prints, so a redirect reproduces the source file minus
//! its metadata comment.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

/// Opens the preamble. Also the marker that says the file is a topic at all.
const PREAMBLE_OPEN: &str = "<!--- stakk-docs";
/// Closes the preamble.
const PREAMBLE_CLOSE: &str = "--->";
/// The keys a preamble may carry.
const KNOWN_KEYS: &[&str] = &["summary"];

struct Topic {
    /// The `stakk docs` topic name — the file stem, unchanged.
    name: String,
    /// The `DocTopic` variant name.
    variant: String,
    /// One line, rendered as clap's possible-value help and in the index.
    summary: String,
    /// The file below its preamble.
    body: String,
}

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env("OUT_DIR"));
    let docs_dir = manifest_dir.join("docs");

    // Catches a file appearing or disappearing; the per-file lines below catch
    // an edit to one that stays. Emitting any of these opts out of cargo's
    // default "rerun on any change in the package".
    println!("cargo::rerun-if-changed=docs");

    let topics = read_topics(&docs_dir);
    assert!(
        !topics.is_empty(),
        "docs/ has no Markdown files, so `stakk docs` would have no topics",
    );

    write_bodies(&out_dir, &topics);
    write_topics_rs(&out_dir, &topics);
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("cargo sets {key} for build scripts"))
}

/// Every `docs/*.md` file as a topic, ordered by name.
///
/// The order is the order `stakk docs` lists topics in, so it is alphabetical
/// rather than the directory's — `read_dir` order is whatever the filesystem
/// says.
fn read_topics(docs_dir: &Path) -> Vec<Topic> {
    let entries = fs::read_dir(docs_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", docs_dir.display()));

    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    paths.sort();

    let mut topics: Vec<Topic> = Vec::new();
    for path in paths {
        println!("cargo::rerun-if-changed={}", path.display());
        topics.push(read_topic(&path));
    }
    topics.sort_by(|a, b| a.name.cmp(&b.name));
    topics
}

fn read_topic(path: &Path) -> Topic {
    let name = path
        .file_stem()
        .expect("a .md path has a stem")
        .to_str()
        .unwrap_or_else(|| fail(path, "the file name is not UTF-8"))
        .to_string();
    let variant = variant_name(path, &name);

    let text =
        fs::read_to_string(path).unwrap_or_else(|e| fail(path, &format!("cannot read: {e}")));
    let (summary, body) = split_preamble(path, &text);

    Topic {
        name,
        variant,
        summary,
        body,
    }
}

/// The `DocTopic` variant for a file stem: `pr-titles` becomes `PrTitles`.
///
/// The stem is restricted to what round-trips through clap's value name, which
/// the generated `#[value(name = ...)]` pins anyway — the point of the check is
/// to reject a name that would produce a broken Rust identifier, with a message
/// naming the file instead of a syntax error in generated code.
fn variant_name(path: &Path, name: &str) -> String {
    let valid = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.contains("--");
    if !valid {
        fail(
            path,
            "the file name must be lowercase letters, digits and single hyphens, starting with a \
             letter (it becomes the `stakk docs <topic>` name)",
        );
    }

    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().expect("no empty words after the check above");
            first.to_ascii_uppercase().to_string() + chars.as_str()
        })
        .collect()
}

/// Split a document into its `summary` and the body below the preamble.
fn split_preamble(path: &Path, text: &str) -> (String, String) {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut opened = false;
    let mut body_start = None;
    let mut consumed = 0usize;

    for line in text.split_inclusive('\n') {
        consumed += line.len();
        let line = line.strip_suffix('\n').unwrap_or(line);

        if !opened {
            if line != PREAMBLE_OPEN {
                fail(
                    path,
                    &format!(
                        "must start with `{PREAMBLE_OPEN}`, the preamble every `stakk docs` topic \
                         carries"
                    ),
                );
            }
            opened = true;
            continue;
        }

        if line == PREAMBLE_CLOSE {
            body_start = Some(consumed);
            break;
        }

        let Some((key, value)) = line.split_once(':') else {
            fail(
                path,
                &format!("preamble line `{line}` is not `key: value` (or `{PREAMBLE_CLOSE}`)"),
            );
        };
        let key = key.trim().to_string();
        let value = value.trim().to_string();

        if !KNOWN_KEYS.contains(&key.as_str()) {
            fail(
                path,
                &format!(
                    "unknown preamble key `{key}` (known keys: {})",
                    KNOWN_KEYS.join(", ")
                ),
            );
        }
        if !seen.insert(key.clone()) {
            fail(path, &format!("duplicate preamble key `{key}`"));
        }
        if value.is_empty() {
            fail(path, &format!("preamble key `{key}` has an empty value"));
        }
        fields.push((key, value));
    }

    let Some(body_start) = body_start else {
        fail(
            path,
            &format!("the preamble is never closed — it needs a `{PREAMBLE_CLOSE}` line"),
        );
    };

    let Some((_, summary)) = fields.into_iter().find(|(key, _)| key == "summary") else {
        fail(
            path,
            "the preamble has no `summary:` line (it is the topic's one-line description in \
             `stakk docs` and `stakk docs --help`)",
        );
    };

    let rest = &text[body_start..];
    let Some(body) = rest.strip_prefix('\n') else {
        fail(
            path,
            &format!("`{PREAMBLE_CLOSE}` must be followed by a blank line, then the document"),
        );
    };
    if body.starts_with('\n') {
        fail(
            path,
            &format!("`{PREAMBLE_CLOSE}` must be followed by exactly one blank line"),
        );
    }
    if body.is_empty() {
        fail(path, "there is nothing below the preamble");
    }

    (summary, body.to_string())
}

fn fail(path: &Path, reason: &str) -> ! {
    // The path is relative to the manifest dir, which is where the reader is.
    let shown = path
        .strip_prefix(env("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display();
    panic!("{shown}: {reason}");
}

/// Write each body to `OUT_DIR`, for the generated `source` to `include_str!`.
///
/// The bodies are copies rather than the originals because what a topic prints
/// is the file *below its preamble*, and `include_str!` has no way to take a
/// slice of a file.
fn write_bodies(out_dir: &Path, topics: &[Topic]) {
    let dir = out_dir.join("docs");
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));

    for topic in topics {
        let path = dir.join(format!("{}.md", topic.name));
        fs::write(&path, &topic.body)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    }
}

fn write_topics_rs(out_dir: &Path, topics: &[Topic]) {
    let mut out = String::new();

    out.push_str(
        "// @generated by build.rs from the docs/ directory. Do not edit.\n\n/// A topic of the \
         documentation bundled into the binary.\n///\n/// One variant per Markdown file in \
         `docs/`, named after the file and\n/// documented with that file's `summary:` preamble \
         line. The doc comments are\n/// load-bearing: clap renders them as possible-value help, \
         and `docs::index`\n/// reads them back to build the topic list.\n#[derive(Debug, Clone, \
         Copy, PartialEq, Eq, clap::ValueEnum)]\npub enum DocTopic {\n",
    );
    for topic in topics {
        writeln!(out, "    /// {}", topic.summary).expect("writing to a String cannot fail");
        writeln!(out, "    #[value(name = \"{}\")]", topic.name)
            .expect("writing to a String cannot fail");
        writeln!(out, "    {},", topic.variant).expect("writing to a String cannot fail");
    }
    out.push_str("}\n\n");

    out.push_str(
        "/// The Markdown for a topic: its file in `docs/`, below the\n/// preamble.\npub(crate) \
         fn source(topic: DocTopic) -> &'static str {\n    match topic {\n",
    );
    for topic in topics {
        writeln!(
            out,
            "        DocTopic::{} => include_str!(concat!(env!(\"OUT_DIR\"), \"/docs/{}.md\")),",
            topic.variant, topic.name,
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("    }\n}\n");

    let path = out_dir.join("doc_topics.rs");
    fs::write(&path, out).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
