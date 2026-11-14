//! The accessible name of one commit row on the Commits tab (#768,
//! docs/0049 D1): a row of plain text makes no a11y node, so the row
//! carries one reading in the order the eye takes it.

use crate::gql;

/// `"Fix the parser — reviewer-a, abcdef1, 3d ago"`; an empty `age` is
/// left out.
pub(super) fn commit_label(commit: &gql::PrCommit, author: &str, age: &str) -> String {
    let sha: String = commit.oid.chars().take(7).collect();
    let mut label = format!("{} — {author}, {sha}", commit.headline);
    if !age.is_empty() {
        label.push_str(", ");
        label.push_str(age);
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit() -> gql::PrCommit {
        gql::PrCommit {
            oid: "abcdef1234567890".into(),
            headline: "Fix the parser".into(),
            committed_at: None,
            author_name: None,
            author_login: None,
            author_avatar: None,
        }
    }

    #[test]
    fn reads_headline_author_sha_then_age() {
        assert_eq!(
            commit_label(&commit(), "reviewer-a", "3d ago"),
            "Fix the parser — reviewer-a, abcdef1, 3d ago"
        );
    }

    #[test]
    fn a_missing_age_is_left_out() {
        assert_eq!(
            commit_label(&commit(), "reviewer-a", ""),
            "Fix the parser — reviewer-a, abcdef1"
        );
    }
}
