//! Shared rail-picker machinery (docs/0022 D5; shared across the issue
//! and PR pages per docs/0043 D6): the draft/checklist state types, the
//! source fetch lifecycle, the pure row builders, save computations and
//! permission gates. Renderers stay page-local — their probe ids, gates
//! and listeners are page-specific (0043 D6), so the replace-semantics
//! logic lives in exactly one place.

use crate::gql;
use crate::service::Kind;

/// One checklist row: the write-target node id plus display data
/// (0022 D5).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickerRow {
    /// Empty for the milestone picker's "No milestone" row — never a
    /// write target (it maps to `milestoneId: null`).
    pub id: String,
    pub label: String,
    /// Assignee rows carry the avatar; label/milestone rows none.
    pub avatar_url: Option<String>,
}

/// Checked flags aligned 1:1 with [`PickerRow`] lists.
#[derive(Debug, PartialEq)]
pub(crate) struct PickerDraft {
    pub rows: Vec<PickerRow>,
    pub checked: Vec<bool>,
}

impl PickerDraft {
    /// docs/0083 D12 (#756): the draft equals what the PR has now, so a
    /// save would post nothing new. Order never matters — the write is a
    /// replace-by-set — and `current` holds the applied node ids.
    pub(crate) fn is_unchanged(&self, current: &[String]) -> bool {
        let mut checked = compute_replace_ids(&self.rows, &self.checked);
        let mut current = current.to_vec();
        checked.sort();
        checked.dedup();
        current.sort();
        current.dedup();
        checked == current
    }
}

/// The picker source fetch lifecycle (0022 D5) — the page fetch-record
/// pattern (docs/0031 `Fetch<T>`), generic over the parsed page.
#[derive(Debug, Default)]
pub(crate) enum PickerFetch<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> PickerFetch<T> {
    pub(crate) fn failed_message(&self) -> Option<&str> {
        match self {
            PickerFetch::Failed(m) => Some(m),
            _ => None,
        }
    }
}

/// Which picker source page to (lazily) fetch (0022 D5). `Reviewers`
/// reads the same `repository.assignableUsers` page as `Assignees` —
/// one fetch, two consumers (docs/0043 D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerSource {
    Labels,
    Assignees,
    Milestones,
    Reviewers,
}

impl PickerSource {
    /// The fetch spec parts: the service kind plus the log name. Both
    /// user pickers map to the same kind, so the in-flight guard
    /// dedupes them per repo (docs/0031 D1).
    pub(crate) fn fetch_kind(self) -> (Kind, &'static str) {
        match self {
            PickerSource::Labels => (Kind::RepoLabels, "repo-labels"),
            PickerSource::Assignees | PickerSource::Reviewers => {
                (Kind::RepoAssignableUsers, "repo-assignable-users")
            }
            PickerSource::Milestones => (Kind::RepoMilestones, "repo-milestones"),
        }
    }
}

// --- pure helpers — table-tested -------------------------------------------

/// D3's arming core (0022 §4, docs/0043 D3): a picker's Edit affordance
/// arms only when the viewer gate holds, every applied node parsed an
/// `id`, and the connection is complete — a save computed from partial
/// state would silently drop entries the page never saw.
pub(crate) fn connection_arming(
    gate: bool,
    total: i64,
    applied_len: usize,
    all_have_ids: bool,
) -> bool {
    gate && all_have_ids && applied_len as i64 == total
}

/// Triage-and-above (0023 D5's lock gate; docs/0043 D4's milestone and
/// reviewer gates): `None` (no explicit permission mapping — implicit
/// read access) means no rights.
pub(crate) fn triage_gate(permission: Option<&str>) -> bool {
    matches!(
        permission,
        Some("ADMIN") | Some("MAINTAIN") | Some("WRITE") | Some("TRIAGE")
    )
}

/// Type-ahead filter for checklist picker rows (#197, amending 0022
/// D6): case-insensitive substring on the display label, whitespace-
/// trimmed. An empty query shows everything — the filter is view-only,
/// it never touches the draft arrays, so `compute_replace_ids` stays
/// 1:1 with the full list and filtered-out checked rows still save.
pub(crate) fn picker_row_matches(query: &str, label: &str) -> bool {
    let needle = query.trim().to_lowercase();
    needle.is_empty() || label.to_lowercase().contains(&needle)
}

/// D3's write: the checked rows' ids are the complete replacement list.
/// Zip-strict — a length mismatch is a render bug, not a partial write.
pub(crate) fn compute_replace_ids(rows: &[PickerRow], checked: &[bool]) -> Vec<String> {
    rows.iter()
        .zip(checked.iter())
        .filter(|(_, c)| **c)
        .map(|(r, _)| r.id.clone())
        .collect()
}

/// The labels checklist: the source page in picker order, then any
/// applied label the source missed (repo page truncation, source still
/// loading) appended — dropping them would silently unlabel on save.
pub(crate) fn label_rows(
    applied: &[gql::IssueLabel],
    source: &[gql::IssueLabel],
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = source
        .iter()
        .map(|l| PickerRow {
            id: l.id.clone().unwrap_or_default(),
            label: l.name.clone(),
            avatar_url: None,
        })
        .collect();
    for l in applied {
        let Some(id) = &l.id else { continue };
        if !rows.iter().any(|r| &r.id == id) {
            rows.push(PickerRow {
                id: id.clone(),
                label: l.name.clone(),
                avatar_url: None,
            });
        }
    }
    rows
}

/// The assignees checklist — same merge as [`label_rows`], logins +
/// avatars for display.
pub(crate) fn assignee_rows(
    applied: &[gql::IssueAssignee],
    source: &[gql::IssueAssignee],
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = source
        .iter()
        .map(|a| PickerRow {
            id: a.id.clone().unwrap_or_default(),
            label: a.login.clone(),
            avatar_url: Some(a.avatar_url.clone()),
        })
        .collect();
    for a in applied {
        let Some(id) = &a.id else { continue };
        if !rows.iter().any(|r| &r.id == id) {
            rows.push(PickerRow {
                id: id.clone(),
                label: a.login.clone(),
                avatar_url: Some(a.avatar_url.clone()),
            });
        }
    }
    rows
}

/// The reviewers checklist (docs/0043 D5): the source users minus the
/// PR author row (GitHub's own picker excludes them), then applied
/// users the source missed appended — same merge-beyond-the-guard rule
/// as [`label_rows`]. Degraded and non-user applied rows (`id: None`)
/// never appear here: the D3 reviewer clause keeps the picker closed
/// whenever one exists.
pub(crate) fn reviewer_rows(
    applied: &[gql::PrReviewRequest],
    source: &[gql::IssueAssignee],
    author_login: Option<&str>,
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = source
        .iter()
        .filter(|u| Some(u.login.as_str()) != author_login)
        .map(|a| PickerRow {
            id: a.id.clone().unwrap_or_default(),
            label: a.login.clone(),
            avatar_url: Some(a.avatar_url.clone()),
        })
        .collect();
    for r in applied {
        let Some(id) = &r.id else { continue };
        if !rows.iter().any(|row| &row.id == id) {
            rows.push(PickerRow {
                id: id.clone(),
                label: r.login.clone(),
                avatar_url: None,
            });
        }
    }
    rows
}

/// The milestone checklist: a leading "No milestone" row (id "" —
/// maps to `milestoneId: null`), then the open-milestone source page,
/// then an applied milestone the source missed (e.g. a closed one).
pub(crate) fn milestone_rows(
    applied: Option<&gql::IssueMilestone>,
    source: &[gql::IssueMilestone],
) -> Vec<PickerRow> {
    let mut rows = vec![PickerRow {
        id: String::new(),
        label: "No milestone".to_string(),
        avatar_url: None,
    }];
    rows.extend(source.iter().map(|m| PickerRow {
        id: m.id.clone(),
        label: m.title.clone(),
        avatar_url: None,
    }));
    if let Some(m) = applied
        && !rows.iter().any(|r| r.id == m.id)
    {
        let label = if m.state.as_deref() == Some("CLOSED") {
            format!("{} · closed", m.title)
        } else {
            m.title.clone()
        };
        rows.push(PickerRow {
            id: m.id.clone(),
            label,
            avatar_url: None,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: &str, name: &str) -> gql::IssueLabel {
        gql::IssueLabel {
            id: Some(id.into()),
            name: name.into(),
            color: None,
        }
    }

    fn user(id: &str, login: &str) -> gql::IssueAssignee {
        gql::IssueAssignee {
            id: Some(id.into()),
            login: login.into(),
            avatar_url: format!("a:{login}"),
        }
    }

    #[test]
    fn connection_arming_table() {
        // gate off, ids missing, truncated connection — each alone disarms.
        assert!(connection_arming(true, 2, 2, true));
        assert!(!connection_arming(false, 2, 2, true));
        assert!(!connection_arming(true, 2, 2, false));
        assert!(!connection_arming(true, 3, 2, true));
    }

    #[test]
    fn triage_gate_table() {
        for (perm, gate) in [
            (Some("ADMIN"), true),
            (Some("MAINTAIN"), true),
            (Some("WRITE"), true),
            (Some("TRIAGE"), true),
            (Some("READ"), false),
            (None, false),
        ] {
            assert_eq!(triage_gate(perm), gate, "{perm:?}");
        }
    }

    #[test]
    fn label_rows_merge_missed_applied() {
        let applied = vec![label("L1", "bug"), label("L2", "rare")];
        let source = vec![label("L1", "bug"), label("L3", "ui")];
        let rows = label_rows(&applied, &source);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["L1", "L3", "L2"],
            "source page first, missed applied appended"
        );
    }

    #[test]
    fn assignee_rows_carry_avatars() {
        let applied = vec![user("U1", "bob")];
        let rows = assignee_rows(&applied, &[]);
        assert_eq!(rows[0].avatar_url.as_deref(), Some("a:bob"));
    }

    #[test]
    fn reviewer_rows_exclude_author_and_merge_missed() {
        let source = vec![user("U1", "bob"), user("U2", "carol")];
        let applied = vec![
            gql::PrReviewRequest {
                id: Some("U3".into()),
                login: "dave".into(),
            },
            // degraded / non-user rows never become candidate rows
            gql::PrReviewRequest {
                id: None,
                login: String::new(),
            },
        ];
        let rows = reviewer_rows(&applied, &source, Some("bob"));
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["U2", "U3"],
            "author excluded, missed applied appended, degraded skipped"
        );
    }

    #[test]
    fn milestone_rows_lead_with_none_and_append_closed() {
        let applied = gql::IssueMilestone {
            id: "M9".into(),
            title: "done".into(),
            state: Some("CLOSED".into()),
        };
        let source = vec![gql::IssueMilestone {
            id: "M1".into(),
            title: "v1".into(),
            state: Some("OPEN".into()),
        }];
        let rows = milestone_rows(Some(&applied), &source);
        assert_eq!(rows[0].id, "", "leading No-milestone row");
        assert_eq!(
            rows[2].label, "done · closed",
            "applied closed milestone appended"
        );
    }

    fn row(id: &str) -> PickerRow {
        PickerRow {
            id: id.into(),
            label: id.into(),
            avatar_url: None,
        }
    }

    #[test]
    fn draft_is_unchanged_when_the_checked_set_equals_the_current_set() {
        let draft = PickerDraft {
            rows: vec![row("a"), row("b"), row("c")],
            checked: vec![true, false, true],
        };
        assert!(draft.is_unchanged(&["c".into(), "a".into()]));
        assert!(!draft.is_unchanged(&["a".into()]));
        assert!(!draft.is_unchanged(&["a".into(), "b".into(), "c".into()]));
    }

    #[test]
    fn an_empty_draft_over_an_empty_set_is_unchanged() {
        let draft = PickerDraft {
            rows: vec![row("a")],
            checked: vec![false],
        };
        assert!(draft.is_unchanged(&[]));
        assert!(!draft.is_unchanged(&["a".into()]));
    }

    #[test]
    fn picker_row_matches_table() {
        // #197: case-insensitive substring; trim; empty query shows all.
        assert!(picker_row_matches("", "bug"));
        assert!(picker_row_matches("   ", "bug"));
        assert!(picker_row_matches("bu", "bug"));
        assert!(picker_row_matches("BUG", "bug"));
        assert!(picker_row_matches("ug", "bug"));
        assert!(!picker_row_matches("fea", "bug"));
        assert!(picker_row_matches(" pat ", "Patrick"));
        assert!(!picker_row_matches("xyz", "Patrick"));
        assert!(picker_row_matches("a", "banana"));
    }

    #[test]
    fn compute_replace_ids_zips_strict() {
        let rows = vec![
            PickerRow {
                id: "A".into(),
                label: "a".into(),
                avatar_url: None,
            },
            PickerRow {
                id: "B".into(),
                label: "b".into(),
                avatar_url: None,
            },
        ];
        assert_eq!(compute_replace_ids(&rows, &[false, true]), vec!["B"]);
        // Zip truncates at the short leg — a length mismatch is a
        // render bug, never a partial write source (D3 note).
        assert_eq!(compute_replace_ids(&rows, &[true]), vec!["A"]);
    }
}
