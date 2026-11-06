//! Paged "list jobs for a workflow run" (#757): `per_page=100` plus
//! `total_count`-driven page walking, so the job-log lookup and the live
//! watch see every job of a big matrix, not GitHub's default first 30.

use std::future::Future;

use crate::api::JobsPage;
use crate::service::SpecResult;

/// Safety cap on pages per run (1000 jobs); `total_count` normally ends
/// the walk long before.
pub(crate) const MAX_JOB_PAGES: u32 = 10;

/// Why the paged walk failed.
pub(crate) enum JobsFetchError {
    /// A network error or non-200 — the raw result, so the caller can
    /// forward it through `apply()` for shared auth/rate-limit handling.
    Failed(Box<SpecResult>),
    Malformed(String),
}

/// URL of page `page` (1-based) given the page-1 URL (which already
/// carries `?per_page=`).
pub(crate) fn page_url(first_url: &str, page: u32) -> String {
    if page <= 1 {
        first_url.to_string()
    } else {
        format!("{first_url}&page={page}")
    }
}

/// Walk the pages via `get` and merge them into one [`JobsPage`]. Stops
/// when `total_count` is reached, a page comes back empty, or the cap hits.
pub(crate) async fn fetch_all_jobs<F, Fut>(
    first_url: &str,
    mut get: F,
) -> Result<JobsPage, JobsFetchError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = SpecResult>,
{
    let mut merged: Option<JobsPage> = None;
    for page in 1..=MAX_JOB_PAGES {
        let result = get(page_url(first_url, page)).await;
        if result.network.is_some() || result.status != 200 {
            return Err(JobsFetchError::Failed(Box::new(result)));
        }
        let parsed: JobsPage = serde_json::from_str(result.body.as_deref().unwrap_or_default())
            .map_err(|e| JobsFetchError::Malformed(e.to_string()))?;
        let empty = parsed.jobs.is_empty();
        let acc = match merged.as_mut() {
            Some(acc) => {
                acc.jobs.extend(parsed.jobs);
                acc
            }
            None => merged.insert(parsed),
        };
        if empty || acc.jobs.len() as u64 >= acc.total_count {
            break;
        }
    }
    Ok(merged.unwrap_or(JobsPage {
        total_count: 0,
        jobs: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Kind;
    use std::cell::RefCell;

    fn ok(body: String) -> SpecResult {
        SpecResult {
            kind: Kind::JobLog,
            ident: None,
            status: 200,
            etag: None,
            rate: None,
            body: Some(body),
            network: None,
            cursor: None,
        }
    }

    /// Serves a run of `total` jobs (`job-0`…) in pages of 100, like GitHub.
    fn serve(total: u64, urls: &RefCell<Vec<String>>, url: String) -> SpecResult {
        urls.borrow_mut().push(url.clone());
        let page: u64 = url.split("&page=").nth(1).map_or(1, |p| p.parse().unwrap());
        let start = (page - 1) * 100;
        let end = (start + 100).min(total);
        let jobs: Vec<String> = (start..end)
            .map(|i| format!(r#"{{"id":{i},"name":"job-{i}"}}"#))
            .collect();
        ok(format!(
            r#"{{"total_count":{total},"jobs":[{}]}}"#,
            jobs.join(",")
        ))
    }

    fn run(total: u64) -> (JobsPage, Vec<String>) {
        let urls = RefCell::new(Vec::new());
        let page =
            futures::executor::block_on(fetch_all_jobs("https://x/jobs?per_page=100", |u| {
                let r = serve(total, &urls, u);
                async move { r }
            }))
            .ok()
            .unwrap();
        (page, urls.into_inner())
    }

    #[test]
    fn up_to_one_page_makes_one_request() {
        for total in [0, 30, 100] {
            let (page, urls) = run(total);
            assert_eq!(page.jobs.len() as u64, total);
            assert_eq!(urls, ["https://x/jobs?per_page=100"], "total {total}");
        }
    }

    #[test]
    fn over_thirty_still_one_request_because_per_page_is_100() {
        let (page, urls) = run(45);
        assert_eq!(urls.len(), 1);
        assert!(page.jobs.iter().any(|j| j.name == "job-44"));
    }

    #[test]
    fn over_a_hundred_finds_a_job_on_page_two_and_three() {
        let (page, urls) = run(101);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[1], "https://x/jobs?per_page=100&page=2");
        assert!(page.jobs.iter().any(|j| j.name == "job-100"));

        let (page, urls) = run(250);
        assert_eq!(urls.len(), 3);
        assert_eq!(page.jobs.len(), 250);
        assert!(page.jobs.iter().any(|j| j.name == "job-249"));
    }

    #[test]
    fn page_cap_bounds_the_walk() {
        let (_, urls) = run(5_000);
        assert_eq!(urls.len() as u32, MAX_JOB_PAGES);
    }

    #[test]
    fn non_200_mid_walk_surfaces_the_failure() {
        let mut n = 0;
        let r = futures::executor::block_on(fetch_all_jobs("https://x/j?per_page=100", |_| {
            n += 1;
            let r = if n == 1 {
                ok(r#"{"total_count":150,"jobs":[{"id":1,"name":"a"}]}"#.into())
            } else {
                SpecResult {
                    status: 403,
                    body: None,
                    ..ok(String::new())
                }
            };
            async move { r }
        }));
        assert!(matches!(r, Err(JobsFetchError::Failed(f)) if f.status == 403));
    }
}
