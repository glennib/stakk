//! Fold Markdown prose to a target width, leaving structure untouched.
//!
//! The docs under `docs/` are written with semantic line breaks (one clause
//! per line), which read as ragged short lines in a terminal. [`wrap_markdown`]
//! joins those clauses back into paragraphs and re-folds them to the width the
//! reader actually has.
//!
//! Fenced code blocks and tables pass through byte-identical: a wrapped shell
//! command is a broken shell command.

use textwrap::Options;
use textwrap::WordSplitter;

use crate::markdown::unwrap::classify;
use crate::markdown::unwrap::fence_marker;
use crate::markdown::unwrap::unwrap_markdown;

/// Re-flow Markdown prose to `width` columns.
///
/// Prose paragraphs and list items are folded; fences, tables, headings,
/// blockquotes, thematic breaks and indented code are emitted verbatim.
pub(crate) fn wrap_markdown(text: &str, width: usize) -> String {
    let joined = unwrap_markdown(text);

    let mut in_fence: Option<String> = None;
    let mut out: Vec<String> = Vec::new();
    // Whether the previous line blocks continuation. Starts true so the first
    // line begins a new block, matching `unwrap_markdown`.
    let mut prev_blocks = true;

    for line in joined.lines() {
        if let Some(marker) = &in_fence {
            out.push(line.to_string());
            if let Some(m) = fence_marker(line)
                && m == *marker
            {
                in_fence = None;
                prev_blocks = true;
            }
            continue;
        }

        if let Some(marker) = fence_marker(line) {
            in_fence = Some(marker);
            out.push(line.to_string());
            prev_blocks = true;
            continue;
        }

        match classify(line, prev_blocks) {
            // Structural and continuation-blocking — emit verbatim.
            Some(true) => {
                out.push(line.to_string());
                prev_blocks = true;
            }
            // List item — continuations hang under the text, not the marker.
            Some(false) => {
                let indent = " ".repeat(list_continuation_indent(line));
                out.push(fold(line, width, &indent));
                prev_blocks = false;
            }
            // Prose — continuations keep the line's own leading indent.
            None => {
                let indent = " ".repeat(leading_spaces(line));
                out.push(fold(line, width, &indent));
                prev_blocks = false;
            }
        }
    }

    out.join("\n")
}

/// Fold one logical line, preserving its leading indent on the first line and
/// applying `subsequent_indent` to every wrapped continuation.
fn fold(line: &str, width: usize, subsequent_indent: &str) -> String {
    let leading = " ".repeat(leading_spaces(line));
    let body = line.trim_start();
    // textwrap treats the text as words, so the original indent has to be
    // reapplied explicitly rather than left in the body.
    let options = Options::new(width)
        .initial_indent(&leading)
        .subsequent_indent(subsequent_indent)
        // Never break at a hyphen. textwrap does by default, which would split
        // `--stack-placement` across lines and read as a different flag.
        .word_splitter(WordSplitter::NoHyphenation);
    textwrap::fill(body, options)
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Width of a list item's marker prefix — leading indent plus the bullet or
/// number and the space after it — so continuation lines align under the
/// item's text.
fn list_continuation_indent(line: &str) -> usize {
    let leading = leading_spaces(line);
    let trimmed = line.trim_start();

    let marker =
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            2
        } else {
            // Ordered item: digits followed by `. ` or `) `.
            let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
            digits + 2
        };

    leading + marker
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises every construct the renderer has to handle. Lives under
    /// `src/`, which rumdl does not format, so its semantic line breaks stay
    /// byte-stable and the snapshots below only move when the renderer does.
    const SAMPLE: &str = include_str!("test_fixtures/render_sample.md");

    #[test]
    fn render_sample_width_80() {
        insta::assert_snapshot!(wrap_markdown(SAMPLE, 80));
    }

    #[test]
    fn render_sample_width_40() {
        insta::assert_snapshot!(wrap_markdown(SAMPLE, 40));
    }

    #[test]
    fn joins_semantic_line_breaks() {
        let input = "One clause.\nAnother clause.\nA third.";
        assert_eq!(
            wrap_markdown(input, 80),
            "One clause. Another clause. A third."
        );
    }

    #[test]
    fn folds_long_prose() {
        let input = "alpha bravo charlie delta echo foxtrot";
        assert_eq!(
            wrap_markdown(input, 20),
            "alpha bravo charlie\ndelta echo foxtrot"
        );
    }

    #[test]
    fn fence_content_is_verbatim() {
        let input = "```\nstakk submit --keep base --new-auto wmtk --dry-run\n```";
        assert_eq!(wrap_markdown(input, 20), input);
    }

    #[test]
    fn table_rows_are_verbatim() {
        let input = "| Flag | Meaning that is quite long indeed \
                     |\n|------|-----------------------------------|";
        assert_eq!(wrap_markdown(input, 20), input);
    }

    #[test]
    fn indented_code_block_is_verbatim() {
        // Inline rather than in the fixture: a Markdown formatter would rewrite
        // an indented block into a fence and silently drop this case.
        let input = "Intro paragraph.\n\n    stakk graph --format=json | jq '.stacks[0]'";
        assert_eq!(wrap_markdown(input, 20), input);
    }

    #[test]
    fn flags_are_never_split_at_a_hyphen() {
        // A folded `--stack-placement` reads as a different flag entirely.
        let wrapped = wrap_markdown("Pass the --stack-placement flag now", 20);
        assert!(
            wrapped.contains("--stack-placement"),
            "flag was split: {wrapped}"
        );
    }

    #[test]
    fn heading_is_verbatim() {
        let input = "## A heading long enough to exceed the width";
        assert_eq!(wrap_markdown(input, 20), input);
    }

    #[test]
    fn list_continuations_hang_under_the_text() {
        let input = "- alpha bravo charlie delta";
        assert_eq!(wrap_markdown(input, 16), "- alpha bravo\n  charlie delta");
    }

    #[test]
    fn ordered_list_continuations_hang_under_the_text() {
        let input = "10. alpha bravo charlie delta";
        assert_eq!(
            wrap_markdown(input, 18),
            "10. alpha bravo\n    charlie delta"
        );
    }

    #[test]
    fn nested_list_keeps_its_indent() {
        let input = "  - alpha bravo charlie delta";
        assert_eq!(
            wrap_markdown(input, 18),
            "  - alpha bravo\n    charlie delta"
        );
    }
}
