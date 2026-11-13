use super::*;
use gpui_component::diff::DiffSide;

fn at(path: &str, line: usize) -> DiffLinePosition {
    DiffLinePosition::new(path, DiffSide::Modified, line)
}

#[test]
fn leaving_a_line_keeps_the_last_one() {
    let a = DiffAnchor::default();
    a.note(Some(&at("a.rs", 7)));
    a.note(None);
    assert_eq!(a.for_path("a.rs").map(|p| p.line()), Some(7));
}

#[test]
fn another_files_line_is_not_returned() {
    let a = DiffAnchor::default();
    a.note(Some(&at("a.rs", 7)));
    assert!(a.for_path("b.rs").is_none());
}

mod page {
    use super::super::super::test_rig::{Rig, server};
    use super::super::super::{Fetch, PrTab};
    use super::at;
    use crate::api::PrFile;
    use gpui::TestAppContext;

    fn lines(n: usize) -> PrFile {
        let body: String = (1..=n).map(|i| format!("+line {i}\n")).collect();
        PrFile {
            filename: "src/main.rs".into(),
            status: "modified".into(),
            additions: n as i64,
            deletions: 0,
            changes: n as i64,
            patch: Some(format!("@@ -0,0 +1,{n} @@\n{body}")),
        }
    }

    /// #762: a changed patch for the same file must bring the reader's
    /// line back instead of leaving the pane at the file header.
    #[gpui::test]
    fn a_changed_patch_restores_the_line_the_reader_was_on(cx: &mut TestAppContext) {
        let rig = Rig::new(cx, server("aaa", "MERGEABLE"), vec![]);
        let (page, vcx) = rig.open(cx);
        page.update(vcx, |p, cx| {
            p.tab = PrTab::Files;
            p.files = Fetch::ready(vec![lines(80)]);
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            assert!(
                p.diff_anchor.restored.borrow().is_empty(),
                "the first swap has no place to return to"
            );

            p.diff_anchor.note(Some(&at("src/main.rs", 60)));
            p.files = Fetch::ready(vec![lines(81)]);
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            let restored = p.diff_anchor.restored.borrow();
            assert_eq!(restored.len(), 1, "the swap scrolls back to the anchor");
            assert_eq!(restored[0].line(), 60);
        });
    }

    #[gpui::test]
    fn another_file_starts_at_its_header(cx: &mut TestAppContext) {
        let rig = Rig::new(cx, server("aaa", "MERGEABLE"), vec![]);
        let (page, vcx) = rig.open(cx);
        page.update(vcx, |p, cx| {
            p.tab = PrTab::Files;
            p.files = Fetch::ready(vec![lines(80)]);
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            p.diff_anchor.note(Some(&at("src/main.rs", 60)));
            let mut other = lines(80);
            other.filename = "src/other.rs".into();
            p.files = Fetch::ready(vec![other]);
            p.selected_file = None;
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            assert!(p.diff_anchor.restored.borrow().is_empty());
            assert!(p.diff_anchor.for_path("src/main.rs").is_none());
        });
    }

    /// A restore must not chase a line the new patch no longer has: the
    /// swap leaves the pane at the file header and nothing panics.
    #[gpui::test]
    fn a_vanished_line_falls_back_to_the_file(cx: &mut TestAppContext) {
        let rig = Rig::new(cx, server("aaa", "MERGEABLE"), vec![]);
        let (page, vcx) = rig.open(cx);
        page.update(vcx, |p, cx| {
            p.tab = PrTab::Files;
            p.files = Fetch::ready(vec![lines(80)]);
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            p.diff_anchor.note(Some(&at("src/main.rs", 70)));
            p.files = Fetch::ready(vec![lines(10)]);
            p.apply_files_payload(cx);
            p.sync_diff(cx);
            assert_eq!(p.diff.read(cx).files().len(), 1);
            assert_eq!(p.diff.read(cx).files()[0].path().as_str(), "src/main.rs");
        });
    }
}
