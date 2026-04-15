/// Strip git commit trailers from a commit description.
///
/// Returns a substring of the input with the trailing trailer block removed, if
/// present. A trailer block is the last paragraph (separated from the rest by
/// one or more blank lines) where every non-empty line matches the standard git
/// trailer format: `Key: value`.
///
/// If the input has only one paragraph, it is returned unchanged — a single
/// paragraph cannot be both content and trailers.
///
/// **Known limitation:** continuation lines (indented follow-up lines within a
/// trailer value) are not recognized. If the last paragraph contains a mix of
/// trailer lines and non-trailer lines (including continuations), the entire
/// paragraph is conservatively kept.
pub(crate) fn strip_trailers(text: &str) -> &str {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return trimmed;
    }

    // Find the boundary between the last paragraph and everything before it.
    // Walk backward from the end to find the first blank line, then skip past
    // any consecutive blank lines to find the end of the preceding content.
    let bytes = trimmed.as_bytes();
    let mut i = bytes.len();

    // Skip backward past the last paragraph (non-blank lines at the end).
    while i > 0 {
        let line_end = i;
        // Find start of this line.
        let line_start = trimmed[..i].rfind('\n').map_or(0, |pos| pos + 1);
        let line = &trimmed[line_start..line_end];
        if line.trim().is_empty() {
            // Hit a blank line — we've found the boundary.
            break;
        }
        i = if line_start == 0 { 0 } else { line_start - 1 };
    }

    // If we walked all the way to the start without finding a blank line, the
    // entire text is one paragraph — return unchanged.
    if i == 0 {
        return trimmed;
    }

    let last_paragraph_start = trimmed[i..].find(|c: char| c != '\n' && c != '\r');
    let last_paragraph_start = match last_paragraph_start {
        Some(offset) => i + offset,
        None => return trimmed,
    };

    let last_paragraph = &trimmed[last_paragraph_start..];

    // Check if every non-empty line in the last paragraph is a trailer.
    let all_trailers = last_paragraph
        .lines()
        .filter(|l| !l.trim().is_empty())
        .all(is_trailer_line);

    if !all_trailers {
        return trimmed;
    }

    // Strip: return everything before the blank-line boundary, trimmed.
    trimmed[..i].trim_end()
}

/// Check if a line matches the git trailer format: `Key: value`.
///
/// The key must start with an ASCII letter and contain only ASCII
/// alphanumerics and hyphens. The separator is `: ` (colon followed by
/// at least one space).
fn is_trailer_line(line: &str) -> bool {
    let Some(colon_pos) = line.find(": ") else {
        return false;
    };
    let key = &line[..colon_pos];
    !key.is_empty()
        && key.starts_with(|c: char| c.is_ascii_alphabetic())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_with_trailers() {
        let input = "feat: add caching\n\nThis adds Redis caching.\n\nSigned-off-by: Alice \
                     <a@b>\nRefs: DAT-123";
        assert_eq!(
            strip_trailers(input),
            "feat: add caching\n\nThis adds Redis caching."
        );
    }

    #[test]
    fn no_trailers() {
        let input = "feat: add caching\n\nThis adds Redis caching.";
        assert_eq!(strip_trailers(input), input);
    }

    #[test]
    fn title_plus_trailers_no_body() {
        let input = "feat: add caching\n\nSigned-off-by: Alice <a@b>";
        assert_eq!(strip_trailers(input), "feat: add caching");
    }

    #[test]
    fn title_only() {
        let input = "feat: add caching";
        assert_eq!(strip_trailers(input), input);
    }

    #[test]
    fn mixed_trailer_and_non_trailer_last_paragraph() {
        let input = "title\n\nbody\n\nSigned-off-by: Alice <a@b>\nThis is not a trailer";
        assert_eq!(strip_trailers(input), input.trim_end());
    }

    #[test]
    fn multiple_paragraphs_only_last_is_trailers() {
        let input = "title\n\nbody\n\nRefs: X\n\nSigned-off-by: Alice <a@b>";
        assert_eq!(strip_trailers(input), "title\n\nbody\n\nRefs: X");
    }

    #[test]
    fn empty_input() {
        assert_eq!(strip_trailers(""), "");
    }

    #[test]
    fn single_paragraph_no_blank_lines() {
        let input = "Signed-off-by: Alice <a@b>\nRefs: DAT-123";
        assert_eq!(strip_trailers(input), input);
    }

    #[test]
    fn multiple_blank_lines_before_trailers() {
        let input = "feat: add caching\n\nBody text.\n\n\nSigned-off-by: Alice <a@b>";
        assert_eq!(strip_trailers(input), "feat: add caching\n\nBody text.");
    }

    #[test]
    fn trailing_whitespace_in_input() {
        let input = "feat: add caching\n\nBody.\n\nRefs: DAT-123\n  ";
        assert_eq!(strip_trailers(input), "feat: add caching\n\nBody.");
    }

    #[test]
    fn co_authored_by_trailer() {
        let input = "fix: resolve deadlock\n\nCo-authored-by: Bob <bob@example.com>";
        assert_eq!(strip_trailers(input), "fix: resolve deadlock");
    }

    #[test]
    fn key_with_no_space_after_colon_is_not_trailer() {
        let input = "title\n\nbody\n\nNot-a-trailer:no space here";
        assert_eq!(strip_trailers(input), input);
    }

    #[test]
    fn key_with_spaces_is_not_trailer() {
        let input = "title\n\nbody\n\nNot a trailer: value here";
        assert_eq!(strip_trailers(input), input);
    }

    #[test]
    fn trailer_value_can_contain_colons() {
        let input = "title\n\nbody\n\nRefs: https://example.com:8080/path";
        assert_eq!(strip_trailers(input), "title\n\nbody");
    }
}
