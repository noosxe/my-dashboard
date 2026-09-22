//! Live debug session model (docs/0055 D1–D5): the pure half — record
//! shape, name validation, directory layout, sway config text. Process
//! spawning, zbus and signalling live in the CLI's `session` module
//! (D14: I/O in the thin bin layer).

use std::path::{Path, PathBuf};

/// A session name MUST match this shape (path safety, greppable
/// receipts — D1).
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    let rest: usize = chars
        .filter(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '-')
        .count();
    let len = name.chars().count();
    rest == 0 && len <= 32
}

/// The managed root all sessions live under (D1):
/// `$XDG_RUNTIME_DIR/dashboard-debug/sessions`, `/tmp` fallback.
pub fn sessions_root() -> PathBuf {
    sessions_root_from(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
}

/// [`sessions_root`] with an explicit `XDG_RUNTIME_DIR` value, so the
/// redirect is testable without touching the environment.
pub fn sessions_root_from(xdg: Option<&std::ffi::OsStr>) -> PathBuf {
    let base = xdg
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("dashboard-debug").join("sessions")
}

/// The on-disk record of one session (D1): everything later verbs must
/// reconstruct, everything the archaeology wanted. Written incrementally
/// during `start` (each milestone updates the file) so a crashed start is
/// still stoppable — hence the `Option` fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    pub name: String,
    pub created_at: u64,
    /// Absolute path of the app binary the session spawned.
    pub binary: String,
    /// Headless output size, `--size WxH`.
    pub size: (u32, u32),
    pub app_pid: Option<u32>,
    pub sway_pid: Option<u32>,
    /// PIDs of a11y bus launchers this session spawned (empty when the
    /// dev shell's bus was already up — D3 kill discipline).
    pub a11y_launcher_pids: Vec<u32>,
    /// The `wayland-N` socket headless sway created (never assumed).
    pub wayland_display: Option<String>,
    /// The `DASHBOARD_STATE_DIR` the app runs under.
    pub state_dir: String,
    /// `ScreenReaderEnabled` sampled before any bridge call could flip
    /// it (D3 restore target). `None` until sampled.
    pub screen_reader_at_start: Option<bool>,
    /// Frame number of the first successful `tree.dump` — the "ready"
    /// watermark.
    pub frame_at_ready: Option<u64>,
}

/// Read a session record; `None` when the file is absent.
pub fn read_record(path: &Path) -> std::io::Result<Option<SessionRecord>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| std::io::Error::other(format!("corrupt session record: {err}"))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Write a session record (pretty JSON — it is meant to be read).
pub fn write_record(path: &Path, record: &SessionRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(record).expect("serialize session record");
    std::fs::write(path, format!("{text}\n"))
}

/// Directory layout of one session (D1). All paths derive from
/// `root/<name>`; `create_all` also makes the artifact subdirs so a
/// session dir is complete from the first write on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDirs {
    pub dir: PathBuf,
}

impl SessionDirs {
    pub fn at(root: impl AsRef<Path>, name: &str) -> Self {
        Self {
            dir: root.as_ref().join(name),
        }
    }

    /// The session's `DASHBOARD_STATE_DIR` — every `AppPaths` root of the
    /// app (including the bridge runtime dir) lands under it.
    pub fn state_dir(&self) -> PathBuf {
        self.dir.join("state")
    }

    pub fn session_json(&self) -> PathBuf {
        self.dir.join("session.json")
    }

    pub fn sway_conf(&self) -> PathBuf {
        self.dir.join("sway.conf")
    }

    /// Process output capture (implementation note to D1: named logs are
    /// what makes a failed `start` diagnosable).
    pub fn app_log(&self) -> PathBuf {
        self.dir.join("app.log")
    }

    pub fn sway_log(&self) -> PathBuf {
        self.dir.join("sway.log")
    }

    pub fn a11y_log(&self) -> PathBuf {
        self.dir.join("a11y.log")
    }

    pub fn screenshots(&self) -> PathBuf {
        self.dir.join("screenshots")
    }

    pub fn snapshots(&self) -> PathBuf {
        self.dir.join("snapshots")
    }

    pub fn transcripts(&self) -> PathBuf {
        self.dir.join("transcripts")
    }

    pub fn create_all(&self) -> std::io::Result<()> {
        for dir in [
            self.state_dir(),
            self.screenshots(),
            self.snapshots(),
            self.transcripts(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// The entire sway config for one session (D2): a headless output of the
/// requested size, nothing else.
pub fn sway_config(width: u32, height: u32) -> String {
    format!("output HEADLESS-1 mode {width}x{height}\n")
}

/// `--size WxH` (D5). Positive dimensions only.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once('x')?;
    let width = width.parse::<u32>().ok()?;
    let height = height.parse::<u32>().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Resolve an executable by name against an explicit `PATH` value (D5's
/// discoverable-failure requirement). `None` when not found.
pub fn find_in_path(name: &str, path_value: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return (Path::new(name).is_file()).then(|| PathBuf::from(name));
    }
    path_value
        .split(':')
        .filter(|entry| !entry.is_empty())
        .find_map(|entry| {
            let candidate = Path::new(entry).join(name);
            candidate.is_file().then_some(candidate)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_shape() {
        for good in ["a", "qa", "bridge-verify", "s1", "0abc", &"x".repeat(32)] {
            assert!(valid_name(good), "{good:?} should be valid");
        }
        for bad in [
            "",
            "-lead",
            "Upper",
            "under_score",
            "sp ace",
            &"x".repeat(33),
            "dot.name",
        ] {
            assert!(!valid_name(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn sessions_root_uses_xdg_runtime_dir_with_tmp_fallback() {
        assert_eq!(
            sessions_root_from(Some("/run/user/1000".as_ref())),
            PathBuf::from("/run/user/1000/dashboard-debug/sessions")
        );
        assert_eq!(
            sessions_root_from(None),
            PathBuf::from("/tmp/dashboard-debug/sessions")
        );
    }

    #[test]
    fn record_round_trips_with_partial_fields() {
        let record = SessionRecord {
            name: "qa".into(),
            created_at: 1739990000,
            binary: "/abs/my-dashboard".into(),
            size: (1920, 1080),
            app_pid: None,
            sway_pid: Some(4241),
            a11y_launcher_pids: vec![4240],
            wayland_display: Some("wayland-3".into()),
            state_dir: "/tmp/qa/state".into(),
            screen_reader_at_start: Some(false),
            frame_at_ready: None,
        };
        let text = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SessionRecord>(&text).unwrap(),
            record
        );
        // The sampled flag is the restore target: it must survive as bool,
        // not stringified.
        assert!(text.contains("\"screen_reader_at_start\":false"));
    }

    #[test]
    fn sway_config_is_one_headless_output_line() {
        assert_eq!(
            sway_config(1920, 1080),
            "output HEADLESS-1 mode 1920x1080\n"
        );
        assert_eq!(sway_config(800, 600), "output HEADLESS-1 mode 800x600\n");
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_size("1x1"), Some((1, 1)));
        for bad in [
            "1920",
            "x1080",
            "1920X1080",
            "0x100",
            "100x0",
            "",
            "axb",
            "-1x5",
        ] {
            assert_eq!(parse_size(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn session_dirs_layout() {
        let dirs = SessionDirs::at("/run/user/1000/dashboard-debug/sessions", "qa");
        assert_eq!(
            dirs.dir,
            PathBuf::from("/run/user/1000/dashboard-debug/sessions/qa")
        );
        assert_eq!(
            dirs.state_dir(),
            PathBuf::from("/run/user/1000/dashboard-debug/sessions/qa/state")
        );
        assert!(dirs.session_json().ends_with("qa/session.json"));
        assert!(dirs.sway_conf().ends_with("qa/sway.conf"));
        assert!(dirs.app_log().ends_with("qa/app.log"));
        assert!(dirs.snapshots().ends_with("qa/snapshots"));
        assert!(dirs.transcripts().ends_with("qa/transcripts"));
        assert!(dirs.screenshots().ends_with("qa/screenshots"));
    }

    #[test]
    fn path_lookup_walks_entries_in_order() {
        let base = std::env::temp_dir().join(format!(
            "dash-debug-path-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = base.join("first");
        let second = base.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("sway"), b"#!/bin/sh\n").unwrap();
        std::fs::write(second.join("grim"), b"#!/bin/sh\n").unwrap();

        let path_value = format!("{}:{}", first.display(), second.display());
        assert_eq!(find_in_path("sway", &path_value), Some(first.join("sway")));
        assert_eq!(find_in_path("grim", &path_value), Some(second.join("grim")));
        assert_eq!(find_in_path("missing", &path_value), None);
        assert_eq!(find_in_path("", &path_value), None);
        // Absolute names bypass the walk.
        let abs = first.join("sway");
        assert_eq!(
            find_in_path(&abs.display().to_string(), "irrelevant"),
            Some(abs)
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
