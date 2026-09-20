//! JSON persistence primitives (docs/0004 §4.2): atomic writes,
//! corrupt-file quarantine, trailing-edge debounced saves.
//!
//! Failure policy: persistence errors are logged and
//! swallowed — a broken disk must never take the UI down (docs/0004
//! §4.7). Loads treat missing files as `None` (defaults win) and park
//! corrupt files as `<name>.invalid-<unix-ms>` instead of crashing.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gpui::App;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Load a JSON file. `Ok(None)` = missing (or corrupt — quarantined,
/// defaults win). Real IO errors surface as `Err`.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => {
            // docs/0008 §5 persistence row: hit at debug.
            log::debug!("state load: {} ({} bytes)", path.display(), bytes.len());
            match serde_json::from_slice(&bytes) {
                Ok(value) => Ok(Some(value)),
                Err(parse_err) => {
                    let quarantined = sibling_with_suffix(path, &format!(".invalid-{}", unix_ms()));
                    match fs::rename(path, &quarantined) {
                        Ok(()) => log::warn!(
                            "corrupt state file {} ({parse_err}); parked as {}",
                            path.display(),
                            quarantined.display()
                        ),
                        Err(rename_err) => log::warn!(
                            "corrupt state file {} ({parse_err}); quarantine failed: {rename_err}",
                            path.display()
                        ),
                    }
                    Ok(None)
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // docs/0008 §5: miss at debug — first boot and sign-out are the
            // normal paths, so this is not a warning.
            log::debug!("state miss: {}", path.display());
            Ok(None)
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!("read {}", path.display()))),
    }
}

/// Pretty JSON written to `<file>.tmp`, fsync'd, then atomically renamed
/// over the target (same-directory rename — atomic on POSIX). Never
/// leaves a partial file under the real name.
pub fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    write_json_atomic(path, &json)
}

/// [`save_json_atomic`]'s write half, shared with the debounced timer
/// task and the quit flush (docs/0004 §4.7) — the payload arrives
/// pre-serialized.
fn write_json_atomic(path: &Path, json: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = sibling_with_suffix(path, ".tmp");
    let written = (|| -> anyhow::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        Ok(())
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    let saved = fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()));
    if saved.is_ok() {
        log::debug!("state saved: {} ({} bytes)", path.display(), json.len());
    }
    saved
}

/// Trailing-edge debounced saver (docs/0004 Q6). gpui tasks cannot be
/// cancelled, so supersession is a shared generation counter: each
/// [`Self::schedule`] bumps it and the only task that still sees itself
/// as the latest one after the delay performs the write — at most one
/// save per quiet window, always the newest value.
///
/// The pending value is serialized at schedule time and parked in the
/// shared inner, so [`Self::flush`] can land it synchronously — the
/// quit path (docs/0045 §9, #283) has no executor to wait on.
///
/// Held by the entity whose state is being saved; call `schedule` from
/// mutation handlers.
#[derive(Clone)]
pub struct DebouncedSave {
    inner: Arc<DebouncedInner>,
}

struct DebouncedInner {
    path: PathBuf,
    delay: Duration,
    generation: AtomicU64,
    /// Pre-serialized payload of the live generation (`None` once
    /// written, flushed, or cancelled).
    pending: std::sync::Mutex<Option<String>>,
}

impl DebouncedSave {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        Self {
            inner: Arc::new(DebouncedInner {
                path,
                delay,
                generation: AtomicU64::new(0),
                pending: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Opt this instance into the process-wide flush-on-quit sweep
    /// ([`flush_pending_on_quit`], docs/0045 §9, #283). Weak refs —
    /// saves whose owners dropped prune lazily at flush time; opt-in
    /// so tests never pollute the registry.
    pub fn register_flush_on_quit(&self) {
        let registry = FLUSHABLES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
        registry.lock().unwrap().push(Arc::downgrade(&self.inner));
    }

    /// Invalidate every pending scheduled save without writing. The next
    /// [`Self::schedule`] starts a fresh quiet window. docs/0045 D3/D5:
    /// submit/discard paths call this before unlinking a draft, so a write
    /// scheduled *before* the clear cannot land *afterwards* and resurrect
    /// a draft for an already-posted comment.
    pub fn cancel(&self) {
        self.inner.generation.fetch_add(1, Ordering::Relaxed);
        *self.inner.pending.lock().unwrap() = None;
    }

    /// Land any pending save synchronously — no timer, no executor.
    /// The quit path calls this for every registered save (docs/0045
    /// §9, #283): a quit landing inside a quiet window must not lose
    /// the tail. A no-op when nothing is pending; after a [`Self::cancel`]
    /// there is nothing pending, so a flush writes nothing. Failures are
    /// logged and swallowed (docs/0004 §4.7).
    pub fn flush(&self) {
        let pending = self.inner.pending.lock().unwrap().take();
        let Some(json) = pending else { return };
        // Kill the timer task — it has nothing left to write, and the
        // file already holds this exact payload.
        self.inner.generation.fetch_add(1, Ordering::Relaxed);
        if let Err(err) = write_json_atomic(&self.inner.path, &json) {
            log::warn!("failed to flush {}: {err}", self.inner.path.display());
        }
    }

    /// Schedule (or reschedule) a save of `value` `delay` from now.
    /// The value is serialized immediately; the timer task only checks
    /// that it is still the latest generation before writing.
    pub fn schedule<T: Serialize + Send + 'static>(&self, value: T, cx: &App) {
        let json = match serde_json::to_string_pretty(&value) {
            Ok(json) => json,
            Err(err) => {
                log::warn!("failed to serialize {}: {err}", self.inner.path.display());
                return;
            }
        };
        *self.inner.pending.lock().unwrap() = Some(json);
        let target_gen = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let inner = self.inner.clone();
        let executor = cx.background_executor().clone();
        let timer_executor = executor.clone();
        executor
            .spawn(async move {
                timer_executor.timer(inner.delay).await;
                if inner.generation.load(Ordering::Relaxed) != target_gen {
                    return; // superseded by a newer schedule
                }
                let pending = inner.pending.lock().unwrap().take();
                let Some(json) = pending else { return };
                if let Err(err) = write_json_atomic(&inner.path, &json) {
                    log::warn!("failed to save {}: {err}", inner.path.display());
                }
            })
            .detach();
    }
}

/// Weak registry of saves that want their pending payload landed at
/// quit. Only [`DebouncedSave::register_flush_on_quit`] feeds it —
/// shell state, settings, and the github draft handles in prod.
static FLUSHABLES: std::sync::OnceLock<std::sync::Mutex<Vec<std::sync::Weak<DebouncedInner>>>> =
    std::sync::OnceLock::new();

/// The on-quit sweep (docs/0045 §9, #283): land every registered
/// save's pending payload synchronously. Prunes saves whose owners
/// dropped; failures log and swallow per docs/0004 §4.7.
pub fn flush_pending_on_quit() {
    let Some(registry) = FLUSHABLES.get() else {
        return;
    };
    let mut guards = registry.lock().unwrap();
    guards.retain(|weak| weak.upgrade().is_some());
    for weak in guards.iter() {
        if let Some(inner) = weak.upgrade() {
            DebouncedSave { inner }.flush();
        }
    }
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state.json".into());
    name.push_str(suffix);
    path.with_file_name(name)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct Sample {
        name: String,
        count: u32,
    }

    fn temp_file(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dash-store-test-{}-{tag}.json", std::process::id()))
    }

    #[test]
    fn roundtrip_missing_then_saved() {
        let path = temp_file("roundtrip");
        std::fs::remove_file(&path).ok();

        assert_eq!(load_json::<Sample>(&path).unwrap(), None);

        let value = Sample {
            name: "shell".into(),
            count: 3,
        };
        save_json_atomic(&path, &value).unwrap();
        assert_eq!(load_json::<Sample>(&path).unwrap().unwrap(), value);

        // Atomicity: no .tmp left behind.
        assert!(!path.with_file_name("roundtrip.json.tmp").exists());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_file_is_quarantined_and_defaults_win() {
        let path = temp_file("corrupt");
        std::fs::write(&path, "{ not json").unwrap();

        assert_eq!(load_json::<Sample>(&path).unwrap(), None);

        // Original gone, quarantine parked with .invalid-<ms> suffix.
        assert!(!path.exists());
        let parent = path.parent().unwrap();
        let parked = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("dash-store-test-") && n.contains(".invalid-"));
        assert!(parked.is_some(), "quarantine file must exist");
        if let Some(parked) = parked {
            std::fs::remove_file(parent.join(parked)).ok();
        }
    }

    /// docs/0045 D3: `cancel()` must kill a pending scheduled write — the
    /// generation contract that keeps a posted comment's draft from being
    /// resurrected by a task scheduled before the submit. The positive
    /// control (schedule without cancel lands) guards against the
    /// assertion passing vacuously.
    #[gpui::test]
    fn cancel_invalidates_a_pending_schedule(cx: &mut gpui::TestAppContext) {
        let base = temp_file("cancel");
        let landed = temp_file("cancel-landed");
        std::fs::remove_file(&base).ok();
        std::fs::remove_file(&landed).ok();
        // Zero delay: the task fires on the first executor drain — no
        // wall-clock waits in tests.
        let save = DebouncedSave::new(base.clone(), Duration::ZERO);
        let control = DebouncedSave::new(landed.clone(), Duration::ZERO);
        let value = Sample {
            name: "x".into(),
            count: 1,
        };
        cx.update(|cx| {
            save.schedule(value.clone(), cx);
            save.cancel();
            control.schedule(value, cx);
        });
        cx.run_until_parked();
        assert!(landed.exists(), "positive control: uncancelled save lands");
        assert!(!base.exists(), "cancelled schedule must not write");
        std::fs::remove_file(&landed).ok();
    }

    // #283 — flush lands the pending payload synchronously: the quit
    // path has no executor to wait on, so this must not need parking.
    #[gpui::test]
    fn flush_lands_a_pending_save_without_waiting(cx: &mut gpui::TestAppContext) {
        let path = temp_file("flush-landed");
        std::fs::remove_file(&path).ok();
        // Production-shaped: a real 500 ms delay the flush must bypass.
        let save = DebouncedSave::new(path.clone(), Duration::from_millis(500));
        cx.update(|cx| {
            save.schedule(
                Sample {
                    name: "tail".into(),
                    count: 2,
                },
                cx,
            );
        });
        assert!(!path.exists(), "nothing written before the quiet window");
        save.flush();
        let on_disk: Option<Sample> = load_json(&path).ok().flatten();
        assert_eq!(on_disk.expect("flushed payload").name, "tail");
        std::fs::remove_file(&path).ok();
    }

    // #283 — cancel then flush: the D5 contract means the pending
    // payload is dead; a quit-time flush must not resurrect it.
    #[gpui::test]
    fn flush_after_cancel_writes_nothing(cx: &mut gpui::TestAppContext) {
        let path = temp_file("flush-cancelled");
        std::fs::remove_file(&path).ok();
        let save = DebouncedSave::new(path.clone(), Duration::ZERO);
        cx.update(|cx| {
            save.schedule(
                Sample {
                    name: "gone".into(),
                    count: 0,
                },
                cx,
            );
            save.cancel();
        });
        save.flush();
        cx.run_until_parked();
        assert!(!path.exists(), "flush after cancel writes nothing");
    }

    // #283 — the flush-all sweep: registered saves land, unregistered
    // ones keep their timer semantics, dropped saves prune instead of
    // crashing the sweep.
    #[gpui::test]
    fn flush_pending_on_quit_flushes_registered_saves(cx: &mut gpui::TestAppContext) {
        let (registered, dropped, unregistered) = (
            temp_file("sweep-registered"),
            temp_file("sweep-dropped"),
            temp_file("sweep-unregistered"),
        );
        std::fs::remove_file(&registered).ok();
        std::fs::remove_file(&dropped).ok();
        std::fs::remove_file(&unregistered).ok();
        let keep = DebouncedSave::new(registered.clone(), Duration::ZERO);
        keep.register_flush_on_quit();
        let leak = DebouncedSave::new(dropped.clone(), Duration::ZERO);
        leak.register_flush_on_quit();
        drop(leak); // owner gone — the sweep must prune, not crash
        let stray = DebouncedSave::new(unregistered.clone(), Duration::ZERO);
        let value = Sample {
            name: "sweep".into(),
            count: 3,
        };
        cx.update(|cx| {
            keep.schedule(value.clone(), cx);
            stray.schedule(value, cx);
        });
        flush_pending_on_quit();
        assert!(registered.exists(), "registered save lands at quit");
        assert!(
            !unregistered.exists(),
            "unregistered saves keep their timer semantics"
        );
        std::fs::remove_file(&registered).ok();
        std::fs::remove_file(&unregistered).ok();
    }

    #[test]
    fn save_creates_missing_parents() {
        let dir =
            std::env::temp_dir().join(format!("dash-store-test-{}-parents", std::process::id()));
        let path = dir.join("nested/deeper/file.json");
        save_json_atomic(
            &path,
            &Sample {
                name: "x".into(),
                count: 0,
            },
        )
        .unwrap();
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
