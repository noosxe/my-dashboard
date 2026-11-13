use super::*;
use crate::gql::{DiffSide, ThreadSubject};

fn thread(id: &str, path: &str) -> PrThread {
    PrThread {
        id: id.into(),
        path: path.into(),
        line: Some(3),
        start_line: None,
        side: DiffSide::Right,
        subject: ThreadSubject::Line,
        is_outdated: false,
        is_resolved: false,
        resolved_by: None,
        viewer_can_reply: true,
        viewer_can_resolve: true,
        viewer_can_unresolve: true,
        comments: Vec::new(),
        comments_total: 0,
    }
}

fn file(path: &str) -> PrFile {
    PrFile {
        filename: path.into(),
        status: "modified".into(),
        additions: 1,
        deletions: 0,
        changes: 1,
        patch: Some("@@ -0,0 +1 @@\n+a".into()),
    }
}

#[test]
fn threads_on_paths_outside_the_list_are_kept() {
    let threads = [thread("t0", "old/name.rs"), thread("t1", "src/a.rs")];
    let got = unplaced(&threads, &[file("src/a.rs")], None);
    assert_eq!(got.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(), vec![0]);
}

#[test]
fn threads_on_the_noticed_file_join_the_bucket() {
    let threads = [thread("t0", "src/a.rs"), thread("t1", "src/b.rs")];
    let files = [file("src/a.rs"), file("src/b.rs")];
    let got = unplaced(&threads, &files, Some("src/a.rs"));
    assert_eq!(got.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(), vec![0]);
}

#[test]
fn nothing_unplaced_when_every_thread_has_a_file() {
    let threads = [thread("t0", "src/a.rs")];
    assert!(unplaced(&threads, &[file("src/a.rs")], None).is_empty());
}
