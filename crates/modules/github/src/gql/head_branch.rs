//! The head branch of a PR and who may delete it (docs/0083 D4, #773).
//! Parsing is tolerant (docs/0007 D3): a missing or malformed field
//! degrades to "no head branch", which hides Delete branch.
//!
//! The gate is not `viewerCanDeleteHeadRef`: its schema description reads
//! "restore the deleted head ref", and it was `false` in every case
//! checked, including an admin on a PR whose head still exists (docs/0083
//! §3.1). The gate is the viewer's permission on the repository that holds
//! the head branch, plus the head not being that repository's default
//! branch. Protected branches aren't filtered up front: a refused delete
//! shows its error under the action row.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `headRef { id name }` with the facts of `headRepository` the gate reads.
/// The head repository is the base repository unless the PR comes from a
/// fork (`isCrossRepository`); either way the viewer needs write access
/// where the branch lives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HeadBranch {
    /// The `Ref` node id `deleteRef` takes.
    pub id: String,
    pub name: String,
    /// `headRepository.viewerPermission`; `None` for a deleted fork.
    pub repo_permission: Option<String>,
    /// `headRepository.defaultBranchRef.name`.
    pub default_branch: Option<String>,
}

impl HeadBranch {
    /// Whether Delete branch may be offered for this branch.
    pub fn deletable(&self) -> bool {
        let can_write = matches!(
            self.repo_permission.as_deref(),
            Some("WRITE" | "MAINTAIN" | "ADMIN")
        );
        can_write && self.default_branch.as_deref() != Some(self.name.as_str())
    }
}

/// `None` once the branch is gone (`headRef` is null) or the node is
/// unusable.
pub(crate) fn parse_head_branch(pr: &Value) -> Option<HeadBranch> {
    let text = |p: &str| {
        pr.pointer(p)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(HeadBranch {
        id: text("/headRef/id")?,
        name: text("/headRef/name")?,
        repo_permission: text("/headRepository/viewerPermission"),
        default_branch: text("/headRepository/defaultBranchRef/name"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr(permission: Option<&str>, default: &str, name: &str) -> Value {
        json!({
            "headRef": {"id": "REF_1", "name": name},
            "headRepository": {
                "viewerPermission": permission,
                "defaultBranchRef": {"name": default},
            },
        })
    }

    #[test]
    fn writers_may_delete_a_feature_branch() {
        for p in ["WRITE", "MAINTAIN", "ADMIN"] {
            let b = parse_head_branch(&pr(Some(p), "main", "feat")).expect("branch");
            assert!(b.deletable(), "{p}");
            assert_eq!(b.id, "REF_1");
            assert_eq!(b.name, "feat");
        }
    }

    #[test]
    fn readers_and_triagers_may_not() {
        for p in [Some("READ"), Some("TRIAGE"), Some("NONE"), None] {
            let b = parse_head_branch(&pr(p, "main", "feat")).expect("branch");
            assert!(!b.deletable(), "{p:?}");
        }
    }

    #[test]
    fn the_default_branch_of_the_head_repository_is_never_offered() {
        let b = parse_head_branch(&pr(Some("ADMIN"), "main", "main")).expect("branch");
        assert!(!b.deletable());
        // A fork whose default branch differs from the base's is judged
        // against the fork's own.
        let b = parse_head_branch(&pr(Some("ADMIN"), "trunk", "main")).expect("branch");
        assert!(b.deletable());
    }

    #[test]
    fn a_fork_the_viewer_cannot_write_is_hidden_and_a_deleted_fork_too() {
        let fork_read = pr(Some("READ"), "main", "patch-1");
        assert!(!parse_head_branch(&fork_read).expect("branch").deletable());
        let deleted_fork = json!({
            "headRef": {"id": "REF_1", "name": "patch-1"},
            "headRepository": null,
        });
        assert!(
            !parse_head_branch(&deleted_fork)
                .expect("branch")
                .deletable()
        );
    }

    #[test]
    fn a_missing_default_branch_does_not_hide_the_button() {
        let pr = json!({
            "headRef": {"id": "REF_1", "name": "feat"},
            "headRepository": {"viewerPermission": "WRITE", "defaultBranchRef": null},
        });
        assert!(parse_head_branch(&pr).expect("branch").deletable());
    }

    #[test]
    fn a_gone_or_malformed_head_ref_is_none() {
        assert_eq!(parse_head_branch(&json!({"headRef": null})), None);
        assert_eq!(parse_head_branch(&json!({})), None);
        assert_eq!(
            parse_head_branch(&json!({"headRef": {"id": "", "name": "x"}})),
            None
        );
        assert_eq!(
            parse_head_branch(&json!({"headRef": {"id": "R", "name": 3}})),
            None
        );
    }
}
