//! A check row's identity (docs/0039 D1, #747). Names aren't unique across
//! a head's checks: two workflows can each have a `build` job, so nothing
//! per-row may key on the name.

use crate::gql::PrCheck;

/// Stable per-row key for the accordion, the pane state and the in-flight
/// arm: the check run's database id. Without one (a payload cached before
/// the id was selected, or a null `databaseId`) it falls back to the
/// (head, name, suite) triple, where the suite stands in for the workflow:
/// each workflow run has its own.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CheckKey(String);

/// What the log chain needs to know about the expanded row.
#[derive(Clone, Debug)]
pub(crate) struct CheckTarget {
    pub key: CheckKey,
    pub name: String,
    pub check_run_id: Option<u64>,
    pub suite_id: Option<u64>,
}

impl CheckTarget {
    pub(crate) fn new(
        head_oid: Option<&str>,
        name: &str,
        check_run_id: Option<u64>,
        suite_id: Option<u64>,
    ) -> Self {
        let key = match check_run_id {
            Some(id) => format!("run:{id}"),
            None => format!(
                "name:{}\0{name}\0{}",
                head_oid.unwrap_or_default(),
                suite_id.unwrap_or_default()
            ),
        };
        Self {
            key: CheckKey(key),
            name: name.to_string(),
            check_run_id,
            suite_id,
        }
    }

    pub(crate) fn of(head_oid: Option<&str>, check: &PrCheck) -> Self {
        Self::new(head_oid, &check.name, check.check_run_id, check.suite_id)
    }

    /// The in-flight and fetch-record identity of this row's chain.
    pub(crate) fn ident(&self, repo: &str, number: i64) -> String {
        format!("{repo}#{number}#{}", self.key.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_wins_and_ignores_the_name() {
        let a = CheckTarget::new(Some("h1"), "build", Some(1), Some(10));
        let renamed = CheckTarget::new(Some("h2"), "compile", Some(1), Some(11));
        let b = CheckTarget::new(Some("h1"), "build", Some(2), Some(10));
        assert_eq!(a.key, renamed.key);
        assert_ne!(a.key, b.key);
    }

    #[test]
    fn the_fallback_separates_head_name_and_suite() {
        let base = CheckTarget::new(Some("h1"), "build", None, Some(10));
        for other in [
            CheckTarget::new(Some("h2"), "build", None, Some(10)),
            CheckTarget::new(Some("h1"), "lint", None, Some(10)),
            CheckTarget::new(Some("h1"), "build", None, Some(11)),
        ] {
            assert_ne!(base.key, other.key);
        }
    }

    #[test]
    fn same_name_rows_get_separate_in_flight_idents() {
        let a = CheckTarget::new(Some("h"), "build", Some(1), None);
        let b = CheckTarget::new(Some("h"), "build", Some(2), None);
        assert_ne!(a.ident("o/r", 7), b.ident("o/r", 7));
    }
}
