//! jj version parsing and the minimum supported version.

use std::fmt;

/// A parsed jj version (`major.minor.patch`).
///
/// Pre-release / build suffixes (e.g. the git hash on dev builds) are tolerated
/// by [`parse`] but not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JjVersion {
    // Field order matters: the derived `Ord` compares lexicographically, so
    // major must come before minor before patch.
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl fmt::Display for JjVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The oldest jj release stakk is tested against.
///
/// This is the latest jj release (0.39.0, 2026-03-04) at the time stakk v1.0.0
/// shipped (2026-03-09). Older versions may still work — for example via user
/// config — but are untested, so we only warn rather than refuse to run.
pub const MIN_SUPPORTED_JJ_VERSION: JjVersion = JjVersion {
    major: 0,
    minor: 39,
    patch: 0,
};

/// Parse the output of `jj --version`.
///
/// Handles plain output (`jj 0.42.0`) and dev builds with a git-hash suffix
/// (`jj 0.42.0-b8f7c455...`). Returns `None` on any output we can't confidently
/// parse, so callers can stay silent rather than nag on unusual builds.
pub fn parse(output: &str) -> Option<JjVersion> {
    // Take the token after the leading "jj" word; tolerant of extra whitespace.
    let token = output.split_whitespace().nth(1)?;
    // Strip a dev-build suffix like "-b8f7c455...".
    let core = token.split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    // Reject trailing junk like "0.42.0.1".
    if parts.next().is_some() {
        return None;
    }
    Some(JjVersion {
        major,
        minor,
        patch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        assert_eq!(
            parse("jj 0.42.0"),
            Some(JjVersion {
                major: 0,
                minor: 42,
                patch: 0
            })
        );
    }

    #[test]
    fn parse_dev_suffix() {
        assert_eq!(
            parse("jj 0.42.0-b8f7c455170e3273897aaf94431f8ccfb1afa7ad"),
            Some(JjVersion {
                major: 0,
                minor: 42,
                patch: 0
            })
        );
    }

    #[test]
    fn parse_trailing_newline() {
        assert_eq!(
            parse("jj 0.42.0\n"),
            Some(JjVersion {
                major: 0,
                minor: 42,
                patch: 0
            })
        );
    }

    #[test]
    fn parse_garbage() {
        assert_eq!(parse("not a version"), None);
    }

    #[test]
    fn parse_empty() {
        assert_eq!(parse(""), None);
    }

    #[test]
    fn parse_partial() {
        assert_eq!(parse("jj 0.42"), None);
    }

    #[test]
    fn parse_extra_component() {
        assert_eq!(parse("jj 0.42.0.1"), None);
    }

    #[test]
    fn ord_compares_correctly() {
        let v038 = JjVersion {
            major: 0,
            minor: 38,
            patch: 99,
        };
        let v039 = JjVersion {
            major: 0,
            minor: 39,
            patch: 0,
        };
        let v100 = JjVersion {
            major: 1,
            minor: 0,
            patch: 0,
        };
        assert!(v038 < v039);
        assert!(v039 < v100);
        assert_eq!(v039.cmp(&v039), std::cmp::Ordering::Equal);
    }

    #[test]
    fn min_version_is_0_39_0() {
        assert_eq!(
            MIN_SUPPORTED_JJ_VERSION,
            JjVersion {
                major: 0,
                minor: 39,
                patch: 0
            }
        );
    }

    #[test]
    fn display_roundtrip() {
        assert_eq!(MIN_SUPPORTED_JJ_VERSION.to_string(), "0.39.0");
    }
}
