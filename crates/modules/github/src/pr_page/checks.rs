//! The Checks tab's pure parts and page plumbing (docs/0016 D7,
//! docs/0039): what a capped list may claim, row identity and order, the
//! summary line, the job-id log cache. `pr_page.rs` keeps only the hooks.

pub(super) mod cap;
pub(super) mod counter;
pub(super) mod header;
pub(crate) mod identity;
pub(crate) mod log_cache;
pub(super) mod order;
pub(crate) mod resolve;
mod row;
mod timing;

#[cfg(test)]
mod tests;
