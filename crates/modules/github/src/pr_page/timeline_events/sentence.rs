//! #911: the words of an event row after the actor's login, one small
//! function per event family. The wording matches the issue page's rows
//! (docs/0021 D3) so the two pages read alike; a label rides in its own slot
//! between lead and tail, rendered as the rail's chip.

use crate::gql::PrEventKind;

/// A label's name and hex colour, for the chip slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LabelRef {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Sentence {
    pub lead: String,
    pub label: Option<LabelRef>,
    /// Text the event replaced, struck through (a renamed title's old one).
    pub struck: Option<String>,
    /// A person or team the event names, in the actor's emphasis.
    pub subject: Option<String>,
    pub tail: String,
}

impl Sentence {
    fn plain(lead: impl Into<String>) -> Self {
        Self {
            lead: lead.into(),
            label: None,
            struck: None,
            subject: None,
            tail: String::new(),
        }
    }

    /// The row as one plain string.
    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        [
            Some(self.lead.as_str()),
            self.label.as_ref().map(|l| l.name.as_str()),
            self.struck.as_deref(),
            self.subject.as_deref(),
            Some(self.tail.as_str()),
        ]
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }
}

pub(super) fn sentence(kind: &PrEventKind, actor: Option<&str>) -> Sentence {
    match kind {
        PrEventKind::Merged => Sentence::plain("merged this"),
        PrEventKind::Closed => Sentence::plain("closed this"),
        PrEventKind::Reopened => Sentence::plain("reopened this"),
        PrEventKind::ForcePushed {
            before: Some(b),
            after: Some(a),
        } => Sentence::plain(format!("force-pushed from {b} to {a}")),
        PrEventKind::ForcePushed { .. } => Sentence::plain("force-pushed the branch"),
        PrEventKind::Labeled { name, color } => label("added the", name, color),
        PrEventKind::Unlabeled { name, color } => label("removed the", name, color),
        PrEventKind::Assigned { assignee } => assignment("assigned", assignee, actor),
        PrEventKind::Unassigned { assignee } => assignment("unassigned", assignee, actor),
        PrEventKind::ReviewRequested { reviewer } => request("requested a review from", reviewer),
        PrEventKind::ReviewRequestRemoved { reviewer } => {
            request("removed the review request for", reviewer)
        }
        PrEventKind::Renamed { from, to } => renamed(from, to),
        PrEventKind::Milestoned { title } => milestone("added this to the", title),
        PrEventKind::Demilestoned { title } => milestone("removed this from the", title),
        PrEventKind::ReadyForReview => Sentence::plain("marked this as ready for review"),
        PrEventKind::ConvertedToDraft => Sentence::plain("marked this as a draft"),
    }
}

/// Review requested / removed: "requested a review from [reviewer]".
fn request(lead: &str, reviewer: &str) -> Sentence {
    Sentence {
        lead: lead.into(),
        label: None,
        struck: None,
        subject: Some(reviewer.into()),
        tail: String::new(),
    }
}

/// Renamed: "changed the title ~~old~~ new", the old text struck through.
fn renamed(from: &str, to: &str) -> Sentence {
    Sentence {
        lead: "changed the title".into(),
        label: None,
        struck: Some(from.into()),
        subject: Some(to.into()),
        tail: String::new(),
    }
}

/// Milestoned / demilestoned: "added this to the [name] milestone".
fn milestone(lead: &str, title: &str) -> Sentence {
    Sentence {
        lead: lead.into(),
        label: None,
        struck: None,
        subject: Some(title.into()),
        tail: "milestone".into(),
    }
}

/// Assigned / unassigned: "assigned bob"; "self-assigned" or "removed their
/// assignment" when the actor is the assignee (GitHub's own wording). The issue page names the login
/// either way; the self case reads better here and costs nothing.
fn assignment(verb: &str, assignee: &str, actor: Option<&str>) -> Sentence {
    if actor == Some(assignee) {
        return Sentence::plain(if verb == "assigned" {
            "self-assigned"
        } else {
            "removed their assignment"
        });
    }
    Sentence {
        lead: verb.into(),
        label: None,
        struck: None,
        subject: Some(assignee.into()),
        tail: String::new(),
    }
}

/// Labeled / unlabeled: "added the [chip] label".
fn label(lead: &str, name: &str, color: &Option<String>) -> Sentence {
    Sentence {
        lead: lead.into(),
        label: Some(LabelRef {
            name: name.into(),
            color: color.clone(),
        }),
        struck: None,
        subject: None,
        tail: "label".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_reads_like_the_issue_page() {
        let k = PrEventKind::Labeled {
            name: "bug".into(),
            color: None,
        };
        assert_eq!(sentence(&k, None).text(), "added the bug label");
        let k = PrEventKind::Unlabeled {
            name: "bug".into(),
            color: None,
        };
        assert_eq!(sentence(&k, None).text(), "removed the bug label");
    }

    #[test]
    fn a_rename_names_both_titles_and_a_milestone_its_name() {
        let k = PrEventKind::Renamed {
            from: "old".into(),
            to: "new".into(),
        };
        assert_eq!(sentence(&k, None).text(), "changed the title old new");
        let k = PrEventKind::Milestoned { title: "v1".into() };
        assert_eq!(sentence(&k, None).text(), "added this to the v1 milestone");
        let k = PrEventKind::Demilestoned { title: "v1".into() };
        assert_eq!(
            sentence(&k, None).text(),
            "removed this from the v1 milestone"
        );
    }

    #[test]
    fn draft_flips_read_like_github() {
        assert_eq!(
            sentence(&PrEventKind::ReadyForReview, None).text(),
            "marked this as ready for review"
        );
        assert_eq!(
            sentence(&PrEventKind::ConvertedToDraft, None).text(),
            "marked this as a draft"
        );
    }

    #[test]
    fn review_requests_name_the_reviewer() {
        let k = PrEventKind::ReviewRequested {
            reviewer: "bob".into(),
        };
        assert_eq!(
            sentence(&k, Some("amy")).text(),
            "requested a review from bob"
        );
        let k = PrEventKind::ReviewRequestRemoved {
            reviewer: "org/core".into(),
        };
        assert_eq!(
            sentence(&k, None).text(),
            "removed the review request for org/core"
        );
    }

    #[test]
    fn assignments_name_the_assignee_or_read_self() {
        let k = PrEventKind::Assigned {
            assignee: "bob".into(),
        };
        assert_eq!(sentence(&k, Some("amy")).text(), "assigned bob");
        assert_eq!(sentence(&k, Some("bob")).text(), "self-assigned");
        assert_eq!(sentence(&k, None).text(), "assigned bob");
        let k = PrEventKind::Unassigned {
            assignee: "bob".into(),
        };
        assert_eq!(sentence(&k, Some("amy")).text(), "unassigned bob");
        assert_eq!(sentence(&k, Some("bob")).text(), "removed their assignment");
    }
}
