//! The Files sidebar row's pieces that are pure (#794).

/// The row's `+adds` and `−dels` text; a side with no lines is absent.
pub(super) fn row_counts(adds: i64, dels: i64) -> (Option<String>, Option<String>) {
    (
        (adds > 0).then(|| format!("+{adds}")),
        (dels > 0).then(|| format!("−{dels}")),
    )
}

#[cfg(test)]
mod tests {
    use super::row_counts;

    #[test]
    fn a_row_hides_a_side_with_no_lines() {
        assert_eq!(row_counts(2, 1), (Some("+2".into()), Some("−1".into())));
        assert_eq!(row_counts(4, 0), (Some("+4".into()), None));
        assert_eq!(row_counts(0, 3), (None, Some("−3".into())));
        assert_eq!(row_counts(0, 0), (None, None));
    }
}
