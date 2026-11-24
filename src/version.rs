//! Version numbers, read and compared.
//!
//! Tags are plain now, but older ones carry a leading `v` - strip it rather than
//! skip the tag. Compare the numbers, not the strings: `1.10` is newer than `1.9`,
//! and a string compare gets that backwards. A missing part counts as zero, so
//! `1.2` equals `1.2.0`.

/// The numeric parts of a version, or `None` if it is not digits and dots.
fn parts(version: &str) -> Option<Vec<u32>> {
    let trimmed = version.trim();
    let digits = trimmed.strip_prefix(&['v', 'V'][..]).unwrap_or(trimmed);
    if digits.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for part in digits.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u32>().ok()?);
    }
    Some(parts)
}

/// True when `candidate` names a later version than `current`. Anything that does
/// not parse is not newer: a tag nobody can read must never turn into a nag.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(mut candidate), Some(mut current)) = (parts(candidate), parts(current)) else {
        return false;
    };
    let length = candidate.len().max(current.len());
    candidate.resize(length, 0);
    current.resize(length, 0);
    candidate > current
}

/// The project version is `major.minor`: Cargo's patch digit is always zero and is
/// never shown anywhere - not in a file name, not in the tag, not on screen.
/// `1.2.0` -> `1.2`.
pub fn short(version: &str) -> String {
    let trimmed = version.trim();
    let mut parts = trimmed.strip_prefix(&['v', 'V'][..]).unwrap_or(trimmed).split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        (Some(only), None) => only.to_string(),
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, short};

    #[test]
    fn the_visible_version_is_major_and_minor() {
        assert_eq!(short("1.2.0"), "1.2");
        assert_eq!(short("2.10.3"), "2.10");
        assert_eq!(short("1.2"), "1.2");
        assert_eq!(short("v1.2.0"), "1.2");
    }

    #[test]
    fn numeric_parts_are_compared_as_numbers() {
        assert!(is_newer("1.10", "1.9"));
        assert!(!is_newer("1.9", "1.10"));
    }

    #[test]
    fn a_missing_part_counts_as_zero() {
        assert!(!is_newer("1.2", "1.2.0"));
        assert!(is_newer("1.2.1", "1.2"));
    }

    #[test]
    fn a_leading_v_is_ignored() {
        assert!(is_newer("v1.2", "1.1"));
    }

    #[test]
    fn unreadable_versions_are_not_newer() {
        assert!(!is_newer("latest", "1.0.0"));
        assert!(!is_newer("1.0.0-beta", "1.0.0"));
        assert!(!is_newer("", "1.0.0"));
    }
}
