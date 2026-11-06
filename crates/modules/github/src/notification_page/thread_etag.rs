//! Transport-level coverage for the notification thread read's
//! conditional revalidation (docs/0038 D4, #803). The etag is stamped by
//! the page's own 200 branch — `GithubState::apply` never sees the body.

#[cfg(test)]
mod tests;
