//! The `dashboard-debug fixture …` verbs (docs/0084 §8, docs/0010 §3):
//! the transport registers one handler with the bridge's control hook
//! ([`dashboard_core::fixture_control`]), which parses the verb's words
//! and acts on the shared state.
//!
//! - `bump [N]` — advance the generation by one, or jump to N.
//! - `budget <remaining> [--budget rest|graphql]` — pin one lane's
//!   `remaining` (REST by default).
//! - `get <path> [--revalidate | --if-none-match <tag>] [--expect <text>]`
//!   — send one REST GET through the transport as the app would, so a
//!   receipt can show 200, 304 and 200 again. `--revalidate` sends the
//!   ETag the last `get` of that path returned; `--expect` fails the
//!   verb unless the 200 body contains the text.
//!
//! Every answer is one JSON object the verb prints.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::FixtureHttp;
use super::rate_headers::Lane;
use crate::http::{Http, Verb};

const BASE: &str = "https://api.github.com";

/// Hook `http` into the bridge. Called once, by the app's own transport.
pub(super) fn register(http: &FixtureHttp) {
    let http = http.clone();
    let last_etags: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    dashboard_core::fixture_control::set_handler(Arc::new(move |args: &[String]| {
        run(&http, &last_etags, args)
    }));
}

fn run(
    http: &FixtureHttp,
    last_etags: &Mutex<HashMap<String, String>>,
    args: &[String],
) -> Result<Value, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["bump"] => Ok(bump(http, None)),
        ["bump", n] => {
            let n = n
                .parse()
                .map_err(|_| format!("bump: {n:?} is not a number"))?;
            Ok(bump(http, Some(n)))
        }
        ["budget", rest @ ..] => budget(http, rest),
        ["get", rest @ ..] => get(http, last_etags, rest),
        _ => Err(USAGE.into()),
    }
}

pub(crate) const USAGE: &str = "fixture bump [N] | budget <remaining> [--budget rest|graphql] \
     | get <path> [--revalidate | --if-none-match <tag>] [--expect <text>]";

fn bump(http: &FixtureHttp, to: Option<u64>) -> Value {
    let generation = http.generation.bump(to);
    log::info!("github fixture: generation {generation}");
    json!({ "generation": generation })
}

fn budget(http: &FixtureHttp, args: &[&str]) -> Result<Value, String> {
    let (remaining, flags) = args.split_first().ok_or(USAGE)?;
    let remaining: u32 = remaining
        .parse()
        .map_err(|_| format!("budget: {remaining:?} is not a number"))?;
    let lane = match flags {
        [] => Lane::Rest,
        ["--budget", name] => Lane::parse(name).ok_or("budget: --budget is rest or graphql")?,
        _ => return Err(USAGE.into()),
    };
    http.budgets.set(lane, remaining);
    let bucket = http.budgets.bucket(lane).ok_or("budget: no bucket")?;
    log::info!(
        "github fixture: {lane:?} budget remaining {}",
        bucket.remaining
    );
    Ok(json!({
        "budget": format!("{lane:?}").to_lowercase(),
        "limit": bucket.limit,
        "remaining": bucket.remaining,
    }))
}

fn get(
    http: &FixtureHttp,
    last_etags: &Mutex<HashMap<String, String>>,
    args: &[&str],
) -> Result<Value, String> {
    let (path, mut flags) = args.split_first().ok_or(USAGE)?;
    let path = path.trim_start_matches('/');
    let mut sent: Option<String> = None;
    let mut expect: Option<&str> = None;
    while let Some((flag, tail)) = flags.split_first() {
        flags = match (*flag, tail) {
            ("--revalidate", tail) => {
                let known = last_etags.lock().unwrap_or_else(|e| e.into_inner());
                sent = Some(
                    known
                        .get(path)
                        .cloned()
                        .ok_or_else(|| format!("get: no earlier ETag for /{path}"))?,
                );
                tail
            }
            ("--if-none-match", [tag, tail @ ..]) => {
                sent = Some((*tag).to_string());
                tail
            }
            ("--expect", [text, tail @ ..]) => {
                expect = Some(text);
                tail
            }
            _ => return Err(USAGE.into()),
        };
    }
    let res = http
        .request(Verb::Get, &format!("{BASE}/{path}"), None, sent.as_deref())
        .map_err(|e| e.0)?;
    if let Some(tag) = &res.etag {
        last_etags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_string(), tag.clone());
    }
    if let Some(text) = expect
        && !res.body.as_deref().is_some_and(|b| b.contains(text))
    {
        return Err(format!(
            "get /{path}: status {}, body does not contain {text:?}",
            res.status
        ));
    }
    Ok(json!({
        "status": res.status,
        "etag": res.etag,
        "body_len": res.body.as_ref().map_or(0, String::len),
        "rate": res.rate.map(|r| json!({
            "limit": r.limit,
            "remaining": r.remaining,
            "reset": r.reset_epoch,
        })),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(tag: &str) -> (std::path::PathBuf, FixtureHttp) {
        let dir = std::env::temp_dir().join(format!("gh-ctl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rest")).unwrap();
        (dir.clone(), FixtureHttp::with_rate(dir, None))
    }

    fn call(
        http: &FixtureHttp,
        etags: &Mutex<HashMap<String, String>>,
        line: &str,
    ) -> Result<Value, String> {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        run(http, etags, &args)
    }

    #[test]
    fn get_walks_200_304_bump_200() {
        let (dir, http) = rig("walk");
        std::fs::write(dir.join("rest/a_b.json"), r#"{"v":"old"}"#).unwrap();
        std::fs::write(dir.join("rest/a_b.g1.json"), r#"{"v":"new"}"#).unwrap();
        let etags = Mutex::default();
        let first = call(&http, &etags, "get a/b --expect old").unwrap();
        assert_eq!(first["status"], 200);
        let again = call(&http, &etags, "get /a/b --revalidate").unwrap();
        assert_eq!(
            (again["status"].as_u64(), again["body_len"].as_u64()),
            (Some(304), Some(0))
        );
        assert_eq!(again["etag"], first["etag"]);
        assert_eq!(call(&http, &etags, "bump").unwrap()["generation"], 1);
        let after = call(&http, &etags, "get a/b --revalidate --expect new").unwrap();
        assert_eq!(after["status"], 200);
        assert_ne!(after["etag"], first["etag"]);
        // A 200 whose body lacks the text fails the verb.
        assert!(call(&http, &etags, "get a/b --expect old").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn budget_and_errors() {
        let (dir, http) = rig("budget");
        let etags = Mutex::default();
        let set = call(&http, &etags, "budget 400 --budget graphql").unwrap();
        assert_eq!(
            (set["budget"].as_str(), set["remaining"].as_u64()),
            (Some("graphql"), Some(400))
        );
        assert_eq!(call(&http, &etags, "bump 5").unwrap()["generation"], 5);
        assert!(call(&http, &etags, "bump x").is_err());
        assert!(call(&http, &etags, "budget").is_err());
        assert!(call(&http, &etags, "budget 1 --budget core").is_err());
        assert!(call(&http, &etags, "get a/b --revalidate").is_err());
        assert!(call(&http, &etags, "nope").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
