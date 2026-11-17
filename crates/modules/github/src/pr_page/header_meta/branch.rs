//! #785: a long branch name in the header's meta line keeps its start
//! (the `feature/` prefix) and its end (the distinguishing suffix) and
//! drops the middle; the full name stays in the tooltip and copies on
//! click.

/// Longest a branch label renders before the middle is dropped.
pub(crate) const MAX_CHARS: usize = 32;

/// `s` unchanged when it fits `max` characters, else its head and tail
/// around a single `…` so the result is exactly `max` characters.
pub(crate) fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 3 {
        return s.to_string();
    }
    let keep = max - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_are_untouched() {
        assert_eq!(middle_truncate("main", MAX_CHARS), "main");
        let exact = "a".repeat(MAX_CHARS);
        assert_eq!(middle_truncate(&exact, MAX_CHARS), exact);
    }

    #[test]
    fn long_names_keep_both_ends_and_the_exact_width() {
        let long = "feature/very-long-branch-name-for-the-manifest-parser-and-loader-overhaul";
        let out = middle_truncate(long, MAX_CHARS);
        assert_eq!(out.chars().count(), MAX_CHARS);
        assert!(out.starts_with("feature/very-"));
        assert!(out.ends_with("loader-overhaul"));
        assert!(out.contains('…'));
    }

    #[test]
    fn multibyte_names_cut_on_char_boundaries() {
        let out = middle_truncate(&"é".repeat(50), 10);
        assert_eq!(out.chars().count(), 10);
    }
}
