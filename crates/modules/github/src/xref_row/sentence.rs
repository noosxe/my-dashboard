//! The words of a cross-reference row (docs/0085 D3): pure functions, so the
//! wording table is tested without a window.

use super::PageKind;
use crate::gql::XrefEvent;

/// "{actor} {verb} {title} {reference}": the actor leads, the title is emphasised,
/// the reference is mono and never breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Sentence {
    pub actor: String,
    /// The verb phrase, e.g. "mentioned this issue in".
    pub verb: String,
    pub title: String,
    pub reference: String,
}

impl Sentence {
    /// The row as one plain string.
    pub(super) fn text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.actor, self.verb, self.title, self.reference
        )
    }
}

pub(super) fn actor(e: &XrefEvent) -> String {
    e.actor_login.clone().unwrap_or_else(|| "Someone".into())
}

/// `#N` in the page's own repository, `owner/repo#N` elsewhere.
pub(super) fn reference(e: &XrefEvent, page_repo: &str) -> String {
    if e.source.repo.eq_ignore_ascii_case(page_repo) {
        format!("#{}", e.source.number)
    } else {
        format!("{}#{}", e.source.repo, e.source.number)
    }
}

/// `willCloseTarget` only means something for a PR source on an issue page;
/// everywhere else the flag is ignored and the mention sentence reads.
pub(super) fn sentence(page: PageKind, e: &XrefEvent, page_repo: &str) -> Sentence {
    let who = actor(e);
    let verb = if e.will_close && e.source.is_pr && page == PageKind::Issue {
        "linked a pull request that will close this issue".to_string()
    } else {
        format!("mentioned this {} in", page.noun())
    };
    Sentence {
        actor: who,
        verb,
        title: e.source.title.clone(),
        reference: reference(e, page_repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::{XrefSource, XrefState};

    fn event(is_pr: bool, repo: &str, will_close: bool, actor: Option<&str>) -> XrefEvent {
        XrefEvent {
            id: "E".into(),
            actor_login: actor.map(str::to_string),
            created_at: None,
            will_close,
            source: XrefSource {
                is_pr,
                number: 42,
                title: "Fix loader".into(),
                repo: repo.into(),
                url: "u".into(),
                state: XrefState::Open,
            },
        }
    }

    const HOME: &str = "fixture-org/beacon";

    #[test]
    fn a_mention_names_the_page_kind() {
        let e = event(true, HOME, false, Some("octo-dev"));
        assert_eq!(
            sentence(PageKind::PullRequest, &e, HOME).text(),
            "octo-dev mentioned this pull request in Fix loader #42"
        );
        assert_eq!(
            sentence(PageKind::Issue, &e, HOME).text(),
            "octo-dev mentioned this issue in Fix loader #42"
        );
    }

    #[test]
    fn will_close_reads_only_for_a_pr_source_on_an_issue_page() {
        let pr = event(true, HOME, true, Some("octo-dev"));
        assert_eq!(
            sentence(PageKind::Issue, &pr, HOME).text(),
            "octo-dev linked a pull request that will close this issue Fix loader #42"
        );
        assert!(
            sentence(PageKind::PullRequest, &pr, HOME)
                .text()
                .contains("mentioned this pull request in")
        );
        let issue = event(false, HOME, true, Some("octo-dev"));
        assert!(
            sentence(PageKind::Issue, &issue, HOME)
                .text()
                .contains("mentioned this issue in")
        );
    }

    #[test]
    fn another_repo_gets_the_full_reference_and_a_null_actor_is_someone() {
        let e = event(false, "fixture-org/atlas", false, None);
        let s = sentence(PageKind::PullRequest, &e, HOME);
        assert_eq!(s.reference, "fixture-org/atlas#42");
        assert_eq!(s.actor, "Someone");
        assert!(s.verb.starts_with("mentioned"));
    }
}
