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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(value)?;
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
/// Held by the entity whose state is being saved; call `schedule` from
/// mutation handlers.
#[derive(Clone)]
pub struct DebouncedSave {
    path: PathBuf,
    delay: Duration,
    generation: Arc<AtomicU64>,
}

impl DebouncedSave {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        Self {
            path,
            delay,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Invalidate every pending scheduled save without writing. The next
    /// [`Self::schedule`] starts a fresh quiet window. docs/0045 D3/D5:
    /// submit/discard paths call this before unlinking a draft, so a write
    /// scheduled *before* the clear cannot land *afterwards* and resurrect
    /// a draft for an already-posted comment.
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Schedule (or reschedule) a save of `value` `delay` from now.
    pub fn schedule<T: Serialize + Send + 'static>(&self, value: T, cx: &App) {
        let target_gen = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let generation = self.generation.clone();
        let path = self.path.clone();
        let delay = self.delay;
        let executor = cx.background_executor().clone();
        let timer_executor = executor.clone();
        executor
            .spawn(async move {
                timer_executor.timer(delay).await;
                if generation.load(Ordering::Relaxed) != target_gen {
                    return; // superseded by a newer schedule
                }
                if let Err(err) = save_json_atomic(&path, &value) {
                    log::warn!("failed to save {}: {err}", path.display());
                }
            })
            .detach();
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
