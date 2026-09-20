//! Draft persistence (docs/0045): one small JSON file per composer
//! context under the module **data** root — user state, not re-derivable
//! cache (the XDG cache root is deleteable; 0024 D7's "the cache holds
//! snapshots" rationale stands, only its no-persistence conclusion is
//! reversed).
//!
//! Layout: `<data>/modules/github/drafts/<sanitized-key>.json` with
//! `{ key, title, body, updated_ms }`. The filename is the sanitized
//! key; sanitized names can theoretically collide, so [`DraftHandle::load`]
//! verifies the payload carries the requested key — a collision can never
//! restore the wrong text, it just fails to restore.
//!
//! Writes ride the core trailing-edge [`DebouncedSave`] (500 ms, the
//! house delay): at most one write per quiet window, and the pending
//! task owns the value, so a page dropped right after the last keystroke
//! still lands it. Submit/discard call [`DraftHandle::discard`] —
//! `cancel()` then unlink — so a write scheduled *before* the clear can
//! never land *afterwards* (0045 D5). Failure policy is the house
//! log-and-swallow (0004 §4.7): a broken disk never takes the UI down.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::App;
use serde::{Deserialize, Serialize};

use dashboard_core::paths::AppPaths;
use dashboard_core::store::{DebouncedSave, load_json};

/// The house debounce delay (shell.json / settings.json precedent).
const DRAFT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// What one draft file holds. `title` is `Some` only for the creators
/// (comment drafts leave it `None`); `updated_ms` is dead weight for v1
/// (a pruning sweep is a 0045 §9 follow-up) but lets that sweep age out
/// stale drafts without a format change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftPayload {
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
    pub body: String,
    pub updated_ms: u64,
}

/// The draft root for the module — one [`DraftHandle`] per composer
/// context. Mirrors [`crate::service::GithubCache`] (injectable root,
/// `at_tmp` for tests).
#[derive(Clone)]
pub struct DraftStore {
    root: PathBuf,
    delay: std::time::Duration,
    /// Prod handles join the quit-time flush sweep (docs/0045 §9,
    /// #283); test stores stay out of the shared registry.
    flush_on_quit: bool,
}

impl DraftStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            root: paths.module_data_dir("github").join("drafts"),
            delay: DRAFT_DEBOUNCE,
            flush_on_quit: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn at_tmp(tag: &str) -> Self {
        Self {
            root: std::env::temp_dir().join(format!(
                "dash-gh-drafts-{tag}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
            // Zero delay under test: the debounced write lands on the
            // first executor drain — no wall-clock waits; prod keeps 500.
            delay: std::time::Duration::ZERO,
            flush_on_quit: false,
        }
    }

    /// Where a key's file lives — test seam so probe fixtures can seed a
    /// draft before the page that would restore it is built.
    #[cfg(test)]
    pub(crate) fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(format!("{}.json", sanitize_key(key)))
    }

    /// The handle for one composer context — key in, save/clear out.
    pub(crate) fn handle(&self, key: String) -> DraftHandle {
        let path = self.root.join(format!("{}.json", sanitize_key(&key)));
        DraftHandle {
            // Prod handles land their pending tail at quit (docs/0045
            // §9, #283).
            save: {
                let save = DebouncedSave::new(path.clone(), self.delay);
                if self.flush_on_quit {
                    save.register_flush_on_quit();
                }
                save
            },
            key,
            path,
        }
    }
}

/// `comment@<owner/repo>#<number>` — issues and PRs share the number
/// space per repo, so one kind is unambiguous (0045 D2).
pub fn comment_key(repo: &str, number: i64) -> String {
    format!("comment@{repo}#{number}")
}

/// `new-issue@<owner/repo>` — the issue creator's draft context.
pub fn new_issue_key(repo: &str) -> String {
    format!("new-issue@{repo}")
}

/// `new-pr@<owner/repo>` — the PR creator's draft context (0045 D7).
pub fn new_pr_key(repo: &str) -> String {
    format!("new-pr@{repo}")
}

/// Filename encoding for a key: `[A-Za-z0-9_-]` kept, everything else
/// collapses to `-` (the `AppPaths::sanitize_id` shape). Collisions are
/// harmless — [`DraftHandle::load`] verifies the payload key.
pub(crate) fn sanitize_key(key: &str) -> String {
    let sanitized: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "draft".into()
    } else {
        sanitized
    }
}

/// One composer's draft: load, debounced schedule, cancel-and-unlink.
/// Cheap to clone nothing — pages own one per surface.
pub struct DraftHandle {
    save: DebouncedSave,
    key: String,
    path: PathBuf,
}

impl DraftHandle {
    /// The draft for this context, if one exists with non-empty content
    /// (0045 D4). A sanitized-name collision (payload key mismatch) or a
    /// wholly-empty payload reads as no draft.
    pub fn load(&self) -> Option<DraftPayload> {
        let payload: Option<DraftPayload> = load_json(&self.path).ok().flatten();
        let payload = payload.filter(|p| p.key == self.key)?;
        let has_content = !payload.body.trim().is_empty()
            || payload
                .title
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty());
        has_content.then_some(payload)
    }

    /// Schedule (or reschedule) the debounced save of this snapshot.
    /// An empty snapshot (no title, no body) discards instead — clearing
    /// the text is the discard gesture (0045 D5) and leaves no residue
    /// file behind.
    pub fn schedule(&self, title: Option<String>, body: String, cx: &App) {
        let empty = body.trim().is_empty() && title.as_deref().is_none_or(|t| t.trim().is_empty());
        if empty {
            self.discard();
            return;
        }
        self.save.schedule(
            DraftPayload {
                key: self.key.clone(),
                title,
                body,
                updated_ms: unix_ms(),
            },
            cx,
        );
    }

    /// Kill pending writes, then unlink. Best-effort: a failed unlink
    /// logs and moves on — the next overwrite or submit-clear fixes it.
    pub fn discard(&self) {
        self.save.cancel();
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => log::warn!("draft discard failed {}: {err}", self.path.display()),
        }
    }

    /// The on-disk path — test seam for seeding/probing files directly.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashboard_core::store::save_json_atomic;

    const KEY: &str = "comment@acme/widgets#42";

    fn handle(store: &DraftStore, key: &str) -> DraftHandle {
        store.handle(key.to_string())
    }

    fn write_payload(path: &std::path::Path, payload: &DraftPayload) {
        save_json_atomic(path, payload).unwrap();
    }

    fn payload(key: &str, title: Option<String>, body: &str) -> DraftPayload {
        DraftPayload {
            key: key.to_string(),
            title,
            body: body.to_string(),
            updated_ms: 1,
        }
    }

    #[test]
    fn keys_follow_the_0045_d2_scheme() {
        assert_eq!(comment_key("acme/widgets", 42), "comment@acme/widgets#42");
        assert_eq!(new_issue_key("acme/widgets"), "new-issue@acme/widgets");
        assert_eq!(new_pr_key("acme/widgets"), "new-pr@acme/widgets");
    }

    #[test]
    fn sanitize_keeps_name_characters_and_collapses_the_rest() {
        assert_eq!(
            sanitize_key("comment@acme/widgets#42"),
            "comment-acme-widgets-42"
        );
        assert_eq!(sanitize_key("new-issue@o.r__x"), "new-issue-o-r__x");
        assert_eq!(sanitize_key("///"), "---");
        assert_eq!(sanitize_key(""), "draft");
    }

    #[test]
    fn roundtrip_loads_the_keyed_payload() {
        let store = DraftStore::at_tmp("roundtrip");
        let h = handle(&store, KEY);
        write_payload(h.path(), &payload(KEY, None, "half-written comment"));
        let loaded = h.load().expect("draft loads");
        assert_eq!(loaded.body, "half-written comment");
        assert_eq!(loaded.title, None);
        assert_eq!(loaded.key, KEY);
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn creators_roundtrip_title_and_body() {
        let store = DraftStore::at_tmp("creator");
        let h = handle(&store, &new_issue_key("acme/widgets"));
        write_payload(
            h.path(),
            &payload(
                &new_issue_key("acme/widgets"),
                Some("A title".into()),
                "A body",
            ),
        );
        let loaded = h.load().expect("creator draft loads");
        assert_eq!(loaded.title.as_deref(), Some("A title"));
        assert_eq!(loaded.body, "A body");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn key_mismatch_reads_as_no_draft_collision_guard() {
        let store = DraftStore::at_tmp("collision");
        let h = handle(&store, KEY);
        // A sanitized-name collision wrote another context's payload here.
        write_payload(
            h.path(),
            &payload("comment@acme/widgets#43", None, "not mine"),
        );
        assert!(h.load().is_none(), "wrong-key payload must not restore");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn empty_payload_reads_as_no_draft() {
        let store = DraftStore::at_tmp("empty");
        let h = handle(&store, KEY);
        write_payload(h.path(), &payload(KEY, None, "   "));
        write_payload(h.path(), &payload(KEY, Some("  ".into()), ""));
        assert!(h.load().is_none(), "whitespace-only draft must not restore");
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn discard_removes_the_file_and_tolerates_a_missing_one() {
        let store = DraftStore::at_tmp("discard");
        let h = handle(&store, KEY);
        write_payload(h.path(), &payload(KEY, None, "bye"));
        h.discard();
        assert!(!h.path().exists(), "discard unlinks");
        h.discard(); // missing file: fine, no panic
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
