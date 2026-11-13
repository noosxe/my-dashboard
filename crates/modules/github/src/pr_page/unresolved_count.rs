//! The unresolved-thread count on the Files sidebar's summary row
//! (#760): a muted second line under "N files changed · +A −D", so the
//! reader sees how much review is still open without scanning every
//! file. Counts every unresolved thread, outdated ones included.
//!
//! Hidden until a read has landed (a wrong "0" would be worse than
//! nothing) and at zero. It reflects the last threads that loaded: a
//! failed refresh keeps the previous data (`Fetch::fail`), so the count
//! stays, while the retry row reports the failure. When the first-100 cap (`threads_cap`)
//! cut threads off the count is a floor and reads "N+ unresolved".

use super::threads_cap::hidden_threads;
use crate::gql::PrThreads;

/// The summary line's text, or `None` when there is nothing to say.
pub(crate) fn label(threads: Option<&PrThreads>) -> Option<String> {
    let t = threads?;
    let n = t.nodes.iter().filter(|t| !t.is_resolved).count();
    if n == 0 {
        return None;
    }
    let plus = if hidden_threads(t) > 0 { "+" } else { "" };
    Some(format!("{n}{plus} unresolved"))
}

#[cfg(test)]
mod tests;
