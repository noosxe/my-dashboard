//! The Checks tab's pure parts and page plumbing (docs/0016 D7,
//! docs/0039): what a capped list may claim, row identity, the job-id log
//! cache. `pr_page.rs` keeps only the render and event hooks.

pub(super) mod cap;
pub(crate) mod identity;
pub(crate) mod log_cache;
pub(crate) mod resolve;

#[cfg(test)]
mod tests;
