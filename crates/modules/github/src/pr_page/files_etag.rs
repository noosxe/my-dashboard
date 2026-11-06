//! Transport-level coverage for the PR Files lane's conditional
//! revalidation (docs/0038 D4, #750). The etag is stamped by the page's
//! own 200 branch — `GithubState::apply` never sees page-owned bodies.

#[cfg(test)]
mod tests;
