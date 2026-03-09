/// Reflow hard-wrapped Markdown text into soft-wrapped paragraphs.
///
/// Joins consecutive prose lines with a space while preserving structural
/// Markdown elements (headers, lists, code blocks, tables, blockquotes,
/// thematic breaks) verbatim.
/// Returns true if the line is a thematic break: 3+ of the same marker
/// char (`-`, `*`, `_`) with only spaces between.
fn is_thematic_break(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let mut chars = trimmed.chars().filter(|c| *c != ' ');
    let first = match chars.next() {
        Some(c) if c == '-' || c == '*' || c == '_' => c,
        _ => return false,
    };
    let count = 1 + chars.filter(|c| *c == first).count();
    // All non-space chars must be the same marker, and there must be 3+.
    trimmed.chars().all(|c| c == first || c == ' ') && count >= 3
}

/// Returns the fence marker if the line opens or closes a fenced code block.
fn fence_marker(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        Some("```".to_string())
    } else if trimmed.starts_with("~~~") {
        Some("~~~".to_string())
    } else {
        None
    }
}

/// Classify a line as structural or prose.
///
/// Returns `Some(true)` for structural lines that block continuation
/// (blank, header, thematic break, code, table, blockquote),
/// `Some(false)` for structural lines that allow continuation (list
/// items), and `None` for prose lines.
fn classify(line: &str, prev_blocks_continuation: bool) -> Option<bool> {
    let trimmed = line.trim_start();
    // Blank line
    if line.trim().is_empty() {
        return Some(true);
    }
    // ATX header
    if trimmed.starts_with('#') {
        return Some(true);
    }
    // Unordered list item — structural but allows continuation
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return Some(false);
    }
    // Ordered list item — structural but allows continuation
    if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(false);
        }
    }
    // Blockquote
    if trimmed.starts_with('>') {
        return Some(true);
    }
    // Thematic break
    if is_thematic_break(line) {
        return Some(true);
    }
    // Table row
    if trimmed.starts_with('|') {
        return Some(true);
    }
    // Indented code block (4+ spaces or tab) — only if previous
    // blocks continuation (not continuing a prose paragraph).
    if prev_blocks_continuation && (line.starts_with("    ") || line.starts_with('\t')) {
        return Some(true);
    }
    // Prose
    None
}

pub(crate) fn unwrap_markdown(text: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        Normal,
        InFence(String), // stores the fence marker (``` or ~~~)
    }

    let mut state = State::Normal;
    let mut output: Vec<String> = Vec::new();

    let lines: Vec<&str> = text.lines().collect();
    // Whether the previous line blocks continuation (joining). Starts true
    // so the first line always begins a new output line.
    let mut prev_blocks = true;

    for line in &lines {
        match &state {
            State::InFence(marker) => {
                output.push(line.to_string());
                if let Some(m) = fence_marker(line)
                    && m == *marker
                {
                    state = State::Normal;
                    prev_blocks = true;
                }
            }
            State::Normal => {
                // Check for fence open.
                if let Some(marker) = fence_marker(line) {
                    state = State::InFence(marker);
                    output.push(line.to_string());
                    prev_blocks = true;
                    continue;
                }

                if let Some(blocks) = classify(line, prev_blocks) {
                    // Structural line — always emitted on its own line.
                    output.push(line.to_string());
                    prev_blocks = blocks;
                } else {
                    // Prose line — join with previous if it allows
                    // continuation.
                    if !prev_blocks && let Some(last) = output.last_mut() {
                        last.push(' ');
                        last.push_str(line.trim());
                        continue;
                    }
                    output.push(line.to_string());
                    prev_blocks = false;
                }
            }
        }
    }

    output.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load a test corpus pair: `{name}.md` (input) and `{name}.expected.md`
    /// (expected output). Trailing newlines are stripped for comparison since
    /// editors tend to add them.
    fn corpus(name: &str) -> (String, String) {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/submit/unwrap/test_corpus");
        let input =
            std::fs::read_to_string(format!("{dir}/{name}.md")).expect("missing input file");
        let expected = std::fs::read_to_string(format!("{dir}/{name}.expected.md"))
            .expect("missing expected file");
        (
            input.trim_end_matches('\n').to_string(),
            expected.trim_end_matches('\n').to_string(),
        )
    }

    #[test]
    fn simple_paragraph() {
        let (input, expected) = corpus("simple_paragraph");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn two_paragraphs() {
        let (input, expected) = corpus("two_paragraphs");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn unordered_list() {
        let (input, expected) = corpus("unordered_list");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn ordered_list() {
        let (input, expected) = corpus("ordered_list");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn fenced_code_block() {
        let (input, expected) = corpus("fenced_code_block");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn tilde_code_block() {
        let (input, expected) = corpus("tilde_code_block");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn indented_code_block() {
        let (input, expected) = corpus("indented_code_block");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn atx_headers() {
        let (input, expected) = corpus("atx_headers");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn blockquotes() {
        let (input, expected) = corpus("blockquotes");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn thematic_break() {
        let (input, expected) = corpus("thematic_break");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn table_rows() {
        let (input, expected) = corpus("table_rows");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn mixed_content() {
        let (input, expected) = corpus("mixed_content");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn already_unwrapped() {
        let (input, expected) = corpus("already_unwrapped");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn empty_input() {
        assert_eq!(unwrap_markdown(""), "");
    }

    #[test]
    fn trailing_leading_whitespace() {
        let (input, expected) = corpus("trailing_leading_whitespace");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn list_item_continuation() {
        let (input, expected) = corpus("list_item_continuation");
        assert_eq!(unwrap_markdown(&input), expected);
    }

    #[test]
    fn nested_fenced_code() {
        let (input, expected) = corpus("nested_fenced_code");
        assert_eq!(unwrap_markdown(&input), expected);
    }
}
