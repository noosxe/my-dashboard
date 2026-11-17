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
    pub tail: String,
}

impl Sentence {
    fn plain(lead: impl Into<String>) -> Self {
        Self {
            lead: lead.into(),
            label: None,
            tail: String::new(),
        }
    }

    /// The row as one plain string.
    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        [
            Some(self.lead.as_str()),
            self.label.as_ref().map(|l| l.name.as_str()),
            Some(self.tail.as_str()),
        ]
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }
}

pub(super) fn sentence(kind: &PrEventKind) -> Sentence {
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
        assert_eq!(sentence(&k).text(), "added the bug label");
        let k = PrEventKind::Unlabeled {
            name: "bug".into(),
            color: None,
        };
        assert_eq!(sentence(&k).text(), "removed the bug label");
    }
}
