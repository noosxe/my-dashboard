//! HTTP transport for the GitHub module (docs/0005 Q4): a minimal injectable
//! `Http` trait so unit tests script responses, and the real `UreqClient`
//! (sync ureq, rustls — runs on gpui's background executor).
//!
//! Security: the token lives in a shared slot owned by the service and is
//! only read per-request by the client; it is sent exclusively to
//! `api.github.com` and never appears in error messages or logs.

use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Token slot shared between the service (writes on connect/sign-out) and
/// the client (reads per request).
pub type SharedToken = Arc<RwLock<Option<String>>>;

/// Rate-limit snapshot from `x-ratelimit-*` headers (docs/0005 §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    /// UTC epoch seconds when the window resets.
    pub reset_epoch: u64,
}

/// One HTTP exchange. `body` is `None` exactly when `status == 304`
/// (ETag revalidation — the payload didn't change).
#[derive(Debug, Clone)]
pub struct HttpResult {
    pub status: u16,
    pub etag: Option<String>,
    pub rate: Option<RateLimit>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Get,
    Post,
}

#[derive(Debug)]
pub struct HttpError(pub String);

pub trait Http: Send + Sync + 'static {
    /// `body` is form-encoded text for the device-flow POSTs (`None` for
    /// GETs); `etag` becomes `If-None-Match` (conditional GET, §3.3).
    fn request(
        &self,
        verb: Verb,
        url: &str,
        body: Option<&str>,
        etag: Option<&str>,
    ) -> Result<HttpResult, HttpError>;
}

const USER_AGENT: &str = concat!("my-dashboard/", env!("CARGO_PKG_VERSION"));
const API_ACCEPT: &str = "application/vnd.github+json";
const OAUTH_ACCEPT: &str = "application/json";

/// Real transport. Sync by design (docs/0005 Q4): calls run on gpui's
/// background executor, never on the main thread.
pub struct UreqClient {
    agent: ureq::Agent,
    token: SharedToken,
}

impl UreqClient {
    pub fn new(token: SharedToken) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build(),
            token,
        }
    }
}

impl Http for UreqClient {
    fn request(
        &self,
        verb: Verb,
        url: &str,
        body: Option<&str>,
        etag: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        let is_api = url.starts_with(crate::api::API_BASE);
        let accept = if is_api { API_ACCEPT } else { OAUTH_ACCEPT };

        let mut req = match verb {
            Verb::Get => self.agent.get(url),
            Verb::Post => self.agent.post(url),
        }
        .set("User-Agent", USER_AGENT)
        .set("Accept", accept);
        if is_api {
            if let Some(token) = self.token.read().ok().and_then(|t| t.clone()) {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
        }
        if let Some(tag) = etag {
            req = req.set("If-None-Match", tag);
        }
        if body.is_some() {
            req = req.set("Content-Type", "application/x-www-form-urlencoded");
        }

        // ureq 2 surfaces >=400 as Err(Error::Status(_, response)) — fold it
        // back into a normal result so callers branch on status, not on
        // transport vs. HTTP errors.
        let dispatched = match body {
            Some(b) => req.send_string(b),
            None => req.call(),
        };
        let response = match dispatched {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => {
                // Transport/DNS/TLS only — ureq 2 error Display never embeds
                // request headers, so the token can't leak through here.
                return Err(HttpError(e.to_string()));
            }
        };

        let status = response.status();
        let etag = response.header("etag").map(str::to_string);
        let rate = RateLimit::from_headers(
            response.header("x-ratelimit-limit"),
            response.header("x-ratelimit-remaining"),
            response.header("x-ratelimit-reset"),
        );
        let body = if status == 304 {
            None
        } else {
            Some(
                response
                    .into_string()
                    .map_err(|e| HttpError(format!("body read failed: {e}")))?,
            )
        };

        Ok(HttpResult {
            status,
            etag,
            rate,
            body,
        })
    }
}

impl RateLimit {
    fn from_headers(
        limit: Option<&str>,
        remaining: Option<&str>,
        reset: Option<&str>,
    ) -> Option<Self> {
        Some(Self {
            limit: limit?.parse().ok()?,
            remaining: remaining?.parse().ok()?,
            reset_epoch: reset?.parse().ok()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Test transport — scripted responses, records requests.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod mock {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::{Http, HttpError, HttpResult, Verb};

    /// A request the mock observed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Recorded {
        pub verb: Verb,
        pub url: String,
        pub body: Option<String>,
        pub etag: Option<String>,
    }

    /// LIFO-scripted responses; records every request.
    pub(crate) struct MockHttp {
        pub responses: Mutex<VecDeque<Result<HttpResult, HttpError>>>,
        pub requests: Mutex<Vec<Recorded>>,
    }

    impl MockHttp {
        pub(crate) fn new(script: Vec<Result<HttpResult, HttpError>>) -> Self {
            Self {
                responses: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// Convenience: one response with a body and no rate headers.
        pub(crate) fn ok(status: u16, body: &str) -> Self {
            Self::new(vec![Ok(HttpResult {
                status,
                etag: Some("\"abc\"".into()),
                rate: None,
                body: Some(body.into()),
            })])
        }

        pub(crate) fn recorded(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Http for MockHttp {
        fn request(
            &self,
            verb: Verb,
            url: &str,
            body: Option<&str>,
            etag: Option<&str>,
        ) -> Result<HttpResult, HttpError> {
            self.requests.lock().unwrap().push(Recorded {
                verb,
                url: url.into(),
                body: body.map(str::to_string),
                etag: etag.map(str::to_string),
            });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockHttp: request made with exhausted script")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratelimit_needs_all_three_headers() {
        assert!(RateLimit::from_headers(Some("5000"), None, None).is_none());
        assert_eq!(
            RateLimit::from_headers(Some("5000"), Some("4999"), Some("1799999999")),
            Some(RateLimit {
                limit: 5000,
                remaining: 4999,
                reset_epoch: 1799999999,
            })
        );
    }

    #[test]
    fn status_304_carries_no_body() {
        // Contract check the service relies on.
        let r = HttpResult {
            status: 304,
            etag: Some("\"x\"".into()),
            rate: None,
            body: None,
        };
        assert!(r.body.is_none() && r.status == 304);
    }
}
