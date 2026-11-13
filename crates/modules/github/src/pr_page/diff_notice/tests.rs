use super::*;
use crate::files::parse_pr_file;

fn file(patch: Option<&str>, changes: i64) -> PrFile {
    PrFile {
        filename: "src/a.rs".into(),
        status: "modified".into(),
        additions: changes,
        deletions: 0,
        changes,
        patch: patch.map(Into::into),
    }
}

fn notice_for(f: &PrFile) -> Option<DiffNotice> {
    classify(f, &parse_pr_file(f))
}

#[test]
fn a_good_patch_draws() {
    let f = file(Some("@@ -0,0 +1,2 @@\n+a\n+b"), 2);
    assert_eq!(notice_for(&f), None);
}

#[test]
fn a_malformed_hunk_header_is_named_not_blank() {
    let f = file(Some("@@ -x,y +z @@\n+a"), 1);
    assert_eq!(notice_for(&f), Some(DiffNotice::Malformed));
}

#[test]
fn a_patch_that_parses_to_nothing_is_malformed() {
    let f = file(Some("this is not a patch"), 3);
    assert_eq!(notice_for(&f), Some(DiffNotice::Malformed));
}

#[test]
fn a_missing_patch_with_changes_is_too_large() {
    assert_eq!(notice_for(&file(None, 4000)), Some(DiffNotice::TooLarge));
}

#[test]
fn a_missing_patch_without_changes_is_no_text_diff() {
    assert_eq!(notice_for(&file(None, 0)), Some(DiffNotice::NoTextDiff));
    assert_eq!(notice_for(&file(Some(""), 0)), Some(DiffNotice::NoTextDiff));
}

#[test]
fn the_row_label_names_the_file() {
    assert_eq!(
        a11y_label(DiffNotice::Malformed, "src/a.rs"),
        "Couldn't render this diff — src/a.rs"
    );
}
