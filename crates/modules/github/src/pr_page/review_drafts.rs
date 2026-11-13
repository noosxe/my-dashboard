//! Bookkeeping for the page-local review drafts (docs/0030 D4): which
//! drafts a confirmed batch consumed (#759) and how many drafts a
//! navigation away would drop (#744). Pure, so both are unit-tested
//! without a window.

use super::LineDraft;

/// Remove exactly the drafts that were in the submitted batch. A draft
/// queued while the batch was in flight is not in `submitted`, so it
/// survives the confirmation. Equal drafts are consumed one for one.
pub(super) fn remove_submitted(drafts: &mut Vec<LineDraft>, submitted: &[LineDraft]) {
    for sent in submitted {
        if let Some(ix) = drafts.iter().position(|d| d == sent) {
            drafts.remove(ix);
        }
    }
}

/// How many unsent review drafts the page holds: queued comments, the
/// finish-review summary and the line composer, each when non-empty.
pub(super) fn draft_count(queued: usize, summary: &str, composer: &str) -> usize {
    queued + usize::from(!summary.trim().is_empty()) + usize::from(!composer.trim().is_empty())
}

/// The discard confirm's question ("Discard 3 review drafts?").
pub(super) fn discard_prompt(count: usize) -> String {
    let noun = if count == 1 { "draft" } else { "drafts" };
    format!("Discard {count} review {noun}?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::DiffSide;

    fn draft(line: u32, body: &str) -> LineDraft {
        LineDraft {
            path: "src/lib.rs".into(),
            side: DiffSide::Right,
            line,
            start: None,
            body: body.into(),
        }
    }

    #[test]
    fn a_draft_queued_mid_submit_survives_the_confirmation() {
        let submitted = vec![draft(1, "a"), draft(2, "b")];
        let mut drafts = submitted.clone();
        drafts.push(draft(3, "queued while submitting"));
        remove_submitted(&mut drafts, &submitted);
        assert_eq!(drafts, vec![draft(3, "queued while submitting")]);
    }

    #[test]
    fn a_confirmed_batch_clears_everything_it_carried() {
        let submitted = vec![draft(1, "a"), draft(2, "b")];
        let mut drafts = submitted.clone();
        remove_submitted(&mut drafts, &submitted);
        assert!(drafts.is_empty());
    }

    #[test]
    fn identical_drafts_are_consumed_one_for_one() {
        let mut drafts = vec![draft(1, "same"), draft(1, "same")];
        remove_submitted(&mut drafts, &[draft(1, "same")]);
        assert_eq!(drafts, vec![draft(1, "same")]);
    }

    #[test]
    fn counts_each_non_empty_kind_once() {
        assert_eq!(draft_count(0, "", ""), 0);
        assert_eq!(draft_count(0, "  \n", " "), 0);
        assert_eq!(draft_count(2, "", ""), 2);
        assert_eq!(draft_count(2, "lgtm", "half a thought"), 4);
        assert_eq!(draft_count(0, "", "typed"), 1);
    }

    #[test]
    fn prompt_agrees_with_the_count() {
        assert_eq!(discard_prompt(1), "Discard 1 review draft?");
        assert_eq!(discard_prompt(3), "Discard 3 review drafts?");
    }
}
