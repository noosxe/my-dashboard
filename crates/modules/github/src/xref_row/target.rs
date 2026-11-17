//! Where a cross-reference row goes when activated (docs/0085 D6): the
//! in-app page for a source in a repository the module lists, the browser
//! (at the URL the query returned) for any other.

use crate::gql::XrefSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    InApp {
        repo: String,
        number: i64,
        is_pr: bool,
    },
    Browser(String),
}

/// `tracked` answers "does the module list this `owner/repo`?". GitHub
/// names are case-insensitive, so the match ignores case.
pub(crate) fn target(source: &XrefSource, tracked: impl Fn(&str) -> bool) -> Target {
    if tracked(&source.repo) {
        Target::InApp {
            repo: source.repo.clone(),
            number: source.number,
            is_pr: source.is_pr,
        }
    } else {
        Target::Browser(source.url.clone())
    }
}

/// The usual `tracked` predicate over a list of repo names.
pub(crate) fn listed<'a>(repos: impl IntoIterator<Item = &'a str>, name: &str) -> bool {
    repos.into_iter().any(|r| r.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::XrefState;

    fn source(repo: &str, is_pr: bool) -> XrefSource {
        XrefSource {
            is_pr,
            number: 42,
            title: "Fix loader".into(),
            repo: repo.into(),
            url: "https://example.com/x/42".into(),
            state: XrefState::Open,
        }
    }

    #[test]
    fn a_tracked_repo_opens_in_app() {
        let t = target(&source("fixture-org/beacon", true), |r| {
            r == "fixture-org/beacon"
        });
        assert_eq!(
            t,
            Target::InApp {
                repo: "fixture-org/beacon".into(),
                number: 42,
                is_pr: true
            }
        );
    }

    #[test]
    fn an_untracked_repo_opens_the_nodes_own_url() {
        let t = target(&source("elsewhere/other", false), |_| false);
        assert_eq!(t, Target::Browser("https://example.com/x/42".into()));
    }

    #[test]
    fn listing_ignores_case() {
        let repos = ["fixture-org/Beacon", "fixture-org/atlas"];
        assert!(listed(repos, "fixture-org/beacon"));
        assert!(!listed(repos, "fixture-org/other"));
    }
}
