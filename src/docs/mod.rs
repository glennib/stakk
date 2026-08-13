//! `stakk docs` — print the documentation bundled into the binary.
//!
//! The topics are the real Markdown files under `docs/`, pulled in with
//! `include_str!`, so there is one source of truth and the text can never
//! describe a build other than the one running it.
//!
//! Output depends on where it is going. At a terminal the prose is re-flowed to
//! the terminal width, because the sources use semantic line breaks that would
//! otherwise read as ragged fragments. Redirected, the source is emitted
//! verbatim — `stakk docs scripting >> AGENTS.md` should write exactly what is
//! in `docs/scripting.md`.

use std::fmt::Write as _;

use clap::ValueEnum;

use crate::cli::DocTopic;
use crate::markdown::wrap::wrap_markdown;

const SCRIPTING: &str = include_str!("../../docs/scripting.md");
const CONFIG: &str = include_str!("../../docs/config.md");
const TEMPLATE: &str = include_str!("../../docs/template.md");

/// Used when stdout is a terminal of unknown size.
const FALLBACK_WIDTH: usize = 80;
/// Prose past this width is tiring to read, however wide the terminal is.
const MAX_WIDTH: usize = 100;

/// The Markdown source for a topic, exactly as it appears in `docs/`.
pub(crate) fn source(topic: DocTopic) -> &'static str {
    match topic {
        DocTopic::Scripting => SCRIPTING,
        DocTopic::Config => CONFIG,
        DocTopic::Template => TEMPLATE,
    }
}

/// The topic list printed by a bare `stakk docs`.
///
/// Generated from the `DocTopic` variants and their doc comments, so it cannot
/// drift from the enum or from `stakk docs --help`.
pub(crate) fn index() -> String {
    let mut out = String::from("stakk documentation topics:\n\n");

    for topic in DocTopic::value_variants() {
        let value = topic
            .to_possible_value()
            .expect("every DocTopic variant has a possible value");
        let help = value
            .get_help()
            .map_or_else(String::new, ToString::to_string);
        writeln!(out, "  {:<10}  {help}", value.get_name())
            .expect("writing to a String cannot fail");
    }

    out.push_str("\nRun `stakk docs <topic>` to print one.\n");
    out.push_str("The same documents live in docs/ at https://github.com/glennib/stakk\n");
    out
}

/// Render a topic.
///
/// `width` is `None` when the output is redirected, which emits the source
/// verbatim; `Some(width)` re-flows the prose for a terminal that wide.
pub(crate) fn render(topic: DocTopic, width: Option<usize>) -> String {
    match width {
        // Verbatim: a redirect must reproduce the source file byte for byte.
        None => source(topic).to_string(),
        Some(width) => wrap_markdown(source(topic), width),
    }
}

/// Print a topic, or the index when no topic is given.
pub(crate) fn print(topic: Option<DocTopic>) {
    let Some(topic) = topic else {
        print!("{}", index());
        return;
    };

    if console::Term::stdout().is_term() {
        println!("{}", render(topic, Some(terminal_width())));
    } else {
        print!("{}", render(topic, None));
    }
}

/// Width to re-flow to.
///
/// `COLUMNS` is checked first because `console` reads the terminal size over
/// ioctl and never consults it — without this the variable would be silently
/// ignored, and there would be no way to exercise a narrow terminal.
fn terminal_width() -> usize {
    if let Ok(value) = std::env::var("COLUMNS")
        && let Ok(columns) = value.trim().parse::<usize>()
        && columns > 0
    {
        return columns.min(MAX_WIDTH);
    }

    console::Term::stdout()
        .size_checked()
        .map_or(FALLBACK_WIDTH, |(_, columns)| columns as usize)
        .min(MAX_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::unwrap::fence_marker;

    /// Longest line permitted inside a fenced block. Fences pass through the
    /// wrapper verbatim by design, so an over-long example would overflow every
    /// narrow terminal. rumdl formats `docs/` at 120 columns and will not catch
    /// this.
    const MAX_FENCE_WIDTH: usize = 76;

    fn all_topics() -> Vec<DocTopic> {
        DocTopic::value_variants().to_vec()
    }

    /// Every line inside a fenced code block, fence markers excluded.
    fn fenced_lines(text: &str) -> Vec<String> {
        let mut open: Option<String> = None;
        let mut lines = Vec::new();

        for line in text.lines() {
            match &open {
                Some(marker) => {
                    if fence_marker(line).as_ref() == Some(marker) {
                        open = None;
                    } else {
                        lines.push(line.to_string());
                    }
                }
                None => open = fence_marker(line),
            }
        }

        lines
    }

    #[test]
    fn redirected_output_is_byte_identical_to_the_source() {
        for topic in all_topics() {
            assert!(!source(topic).is_empty(), "{topic:?} source is empty");
            assert_eq!(
                render(topic, None),
                source(topic),
                "{topic:?} is not reproduced verbatim when redirected"
            );
        }
    }

    #[test]
    fn terminal_output_is_reflowed() {
        // Guards against the TTY path silently degrading to verbatim: the docs
        // use semantic line breaks, so folding them must change the text.
        for topic in all_topics() {
            assert_ne!(
                render(topic, Some(80)),
                source(topic),
                "{topic:?} was not re-flowed at width 80"
            );
        }
    }

    #[test]
    fn fenced_content_survives_wrapping_verbatim() {
        for topic in all_topics() {
            let src = source(topic);
            let expected = fenced_lines(src);
            for width in [40, 60, 80, 120] {
                let wrapped = wrap_markdown(src, width);
                assert_eq!(
                    fenced_lines(&wrapped),
                    expected,
                    "{topic:?} fenced content changed at width {width}"
                );
            }
        }
    }

    #[test]
    fn fenced_lines_fit_a_narrow_terminal() {
        for topic in all_topics() {
            for line in fenced_lines(source(topic)) {
                assert!(
                    line.chars().count() <= MAX_FENCE_WIDTH,
                    "{topic:?}: fenced line is {} chars (max {MAX_FENCE_WIDTH}): {line}",
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn index_lists_every_topic() {
        let index = index();
        for topic in all_topics() {
            let name = topic
                .to_possible_value()
                .expect("variant has a possible value")
                .get_name()
                .to_string();
            assert!(index.contains(&name), "index is missing {name}");
        }
    }

    #[test]
    fn index_describes_scripting_for_coding_agents() {
        assert!(
            index().contains("coding agents"),
            "the scripting entry should name coding agents, so an agent scanning the index picks \
             it"
        );
    }

    #[test]
    fn columns_overrides_the_detected_width() {
        // Serialized implicitly: no other test touches COLUMNS.
        unsafe { std::env::set_var("COLUMNS", "37") };
        let width = terminal_width();
        unsafe { std::env::remove_var("COLUMNS") };
        assert_eq!(width, 37);
    }
}
