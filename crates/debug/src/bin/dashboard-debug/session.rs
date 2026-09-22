//! `session` verbs (docs/0055 D1–D5): the I/O half of live debug
//! sessions. Pure record/layout logic lives in
//! [`dashboard_debug::session`]; this module spawns, polls, signals and
//! talks to the session bus. Nothing here touches the host compositor or
//! the host pointer/keyboard — headless sway is the only window system
//! involved (D2), and only PIDs recorded as spawned-by-us are ever
//! signalled (D3).

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dashboard_debug::cli::SessionCmd;
use dashboard_debug::session::{
    SessionDirs, SessionRecord, find_in_path, read_record, sessions_root, sway_config, valid_name,
    write_record,
};
use serde_json::json;

use crate::Conn;

/// How long [`start`] waits for sway's `wayland-N` socket to appear (D2).
const SWAY_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
/// Bridge readiness poll cadence (D5: socket + auth + `window.list`).
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn run(cmd: SessionCmd, global_session: Option<String>) -> i32 {
    if let Some(name) = global_session {
        eprintln!(
            "--session {name:?} binds bridge verbs, not session verbs — \
             session verbs name their target themselves"
        );
        return 2;
    }
    match cmd {
        SessionCmd::Start {
            name,
            binary,
            size,
            timeout_secs,
        } => start(
            &name.unwrap_or_else(|| "default".into()),
            binary.as_deref(),
            size,
            timeout_secs,
        ),
        SessionCmd::Status { name } => status(name.as_deref()),
        SessionCmd::Stop { name, keep } => stop(&name, keep),
        SessionCmd::Env { name } => env(&name),
    }
}

/// D4: redirect this process's roots to the session's state dir and
/// verify the bridge socket. Called before any token/socket lookup.
pub fn bind(name: &str) -> Result<(), i32> {
    if !valid_name(name) {
        eprintln!("invalid session name {name:?} — must match ^[a-z0-9][a-z0-9-]{{0,31}}$");
        return Err(2);
    }
    let dirs = SessionDirs::at(sessions_root(), name);
    let record = match read_record(&dirs.session_json()) {
        Ok(Some(record)) => record,
        Ok(None) => {
            eprintln!(
                "no session {name:?} at {} — start one: dashboard-debug session start --name {name}",
                dirs.dir.display()
            );
            return Err(1);
        }
        Err(err) => {
            eprintln!("{}: {err}", dirs.session_json().display());
            return Err(1);
        }
    };
    set_state_dir_env(&record.state_dir);
    let socket = dashboard_core::AppPaths::init()
        .runtime_dir()
        .join("debug.sock");
    if !socket.exists() {
        eprintln!(
            "session {name:?}: no bridge socket at {} — the app is down \
             (`dashboard-debug session status {name}` to inspect)",
            socket.display()
        );
        return Err(1);
    }
    Ok(())
}

fn set_state_dir_env(state_dir: &str) {
    // SAFETY: called on the main thread before any threads exist (no zbus
    // connection, no spawned children) — no concurrent env readers.
    unsafe { std::env::set_var("DASHBOARD_STATE_DIR", state_dir) };
}

/// Everything `start` owns, so every failure path funnels through one
/// unwind: kill what was started, restore the recorded flag, remove the
/// session dir (D5).
struct StartGuard {
    dirs: SessionDirs,
    record: SessionRecord,
}

impl StartGuard {
    fn checkpoint(&mut self) {
        if let Err(err) = write_record(&self.dirs.session_json(), &self.record) {
            eprintln!("warning: could not update session record: {err}");
        }
    }

    fn fail(mut self, step: &str) -> i32 {
        eprintln!("start failed at: {step}");
        run_stop(
            &mut LiveActor { report: false },
            &mut self.record,
            &stop_steps(true),
        );
        let _ = std::fs::remove_dir_all(&self.dirs.dir);
        1
    }
}

fn start(name: &str, binary: Option<&str>, size: (u32, u32), timeout_secs: u64) -> i32 {
    if !valid_name(name) {
        eprintln!("invalid session name {name:?} — must match ^[a-z0-9][a-z0-9-]{{0,31}}$");
        return 2;
    }
    let Some(xdg_runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        eprintln!("XDG_RUNTIME_DIR is not set — headless sway creates its socket there");
        return 1;
    };
    let xdg_runtime = PathBuf::from(xdg_runtime);
    let path_env = std::env::var("PATH").unwrap_or_default();
    if find_in_path("sway", &path_env).is_none() {
        eprintln!(
            "sway not found on PATH — enter the dev shell (nix develop) or install sway (docs/0055 §5)"
        );
        return 1;
    }
    let binary = binary.unwrap_or("target/debug/my-dashboard");
    let Ok(binary_path) = std::fs::canonicalize(binary) else {
        eprintln!("app binary {binary:?} not found — build it or pass --binary <path>");
        return 1;
    };

    let dirs = SessionDirs::at(sessions_root(), name);
    if dirs.dir.exists() {
        eprintln!(
            "session {name:?} already exists at {} — inspect with \
             `dashboard-debug session status {name}`, remove with \
             `dashboard-debug session stop {name}`",
            dirs.dir.display()
        );
        return 1;
    }
    if let Err(err) = dirs.create_all() {
        eprintln!("cannot create {}: {err}", dirs.dir.display());
        return 1;
    }

    // Redirect this process's roots first, so the readiness poll below
    // resolves the session runtime dir (the app gets the same value via
    // its environment). See set_state_dir_env for the safety argument —
    // still single-threaded here.
    set_state_dir_env(&dirs.state_dir().display().to_string());

    let mut guard = StartGuard {
        dirs,
        record: SessionRecord {
            name: name.into(),
            created_at: unix_now(),
            binary: binary_path.display().to_string(),
            size,
            app_pid: None,
            sway_pid: None,
            a11y_launcher_pids: Vec::new(),
            wayland_display: None,
            state_dir: String::new(),
            screen_reader_at_start: None,
            frame_at_ready: None,
        },
    };
    guard.record.state_dir = guard.dirs.state_dir().display().to_string();
    guard.checkpoint();

    // D5 order: a11y bus → record ScreenReaderEnabled → sway → app.
    match ensure_a11y_bus(&guard.dirs) {
        Ok(pids) => guard.record.a11y_launcher_pids = pids,
        Err(step) => return guard.fail(&step),
    }
    guard.checkpoint();

    match read_screen_reader_enabled() {
        Ok(flag) => guard.record.screen_reader_at_start = Some(flag),
        Err(step) => return guard.fail(&step),
    }
    guard.checkpoint();

    if let Err(err) = std::fs::write(guard.dirs.sway_conf(), sway_config(size.0, size.1)) {
        return guard.fail(&format!("writing sway.conf: {err}"));
    }
    let before_sockets = wayland_sockets(&xdg_runtime);
    let sway_log = log_file(&guard.dirs.sway_log());
    let sway = Command::new("sway")
        .arg("-c")
        .arg(guard.dirs.sway_conf())
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .stdin(Stdio::null())
        .stdout(dup(&sway_log))
        .stderr(sway_log)
        .spawn();
    let sway = match sway {
        Ok(child) => child,
        Err(err) => return guard.fail(&format!("spawning sway: {err}")),
    };
    guard.record.sway_pid = Some(sway.id());
    guard.checkpoint();

    let display = match wait_for_new_socket(&xdg_runtime, &before_sockets, SWAY_SOCKET_TIMEOUT) {
        Ok(display) => display,
        Err(step) => return guard.fail(&step),
    };
    guard.record.wayland_display = Some(display.clone());
    guard.checkpoint();

    let app_log = log_file(&guard.dirs.app_log());
    let app = Command::new(&binary_path)
        .env("DASHBOARD_STATE_DIR", &guard.record.state_dir)
        .env("WAYLAND_DISPLAY", &display)
        .stdin(Stdio::null())
        .stdout(dup(&app_log))
        .stderr(app_log)
        .spawn();
    let app = match app {
        Ok(child) => child,
        Err(err) => return guard.fail(&format!("spawning app {}: {err}", binary_path.display())),
    };
    guard.record.app_pid = Some(app.id());
    guard.checkpoint();

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let deadline = Instant::now() + timeout;
    let (conn, frame) = loop {
        if let Some(ready) = try_bridge_ready() {
            break ready;
        }
        if Instant::now() >= deadline {
            let step = format!(
                "bridge not ready within {}s (app log: {})",
                timeout_secs,
                guard.dirs.app_log().display()
            );
            return guard.fail(&step);
        }
        std::thread::sleep(BRIDGE_POLL_INTERVAL);
    };

    guard.record.frame_at_ready = frame;
    guard.checkpoint();

    // One deliberate tree.dump marks the session activated (it flips
    // ScreenReaderEnabled on the host bus — recorded, restored by `stop`).
    drop(conn);
    println!("session {} ready", guard.record.name);
    println!("  state dir: {}", guard.record.state_dir);
    println!(
        "  display:   {} (headless sway, pid {})",
        display,
        pid_text(guard.record.sway_pid)
    );
    println!(
        "  app:       pid {} ({})",
        pid_text(guard.record.app_pid),
        guard.record.binary
    );
    println!("  frame:     {}", frame_text(frame));
    println!(
        "  drive it:  dashboard-debug --session {} tree",
        guard.record.name
    );
    0
}

/// Duplicate a fresh log file handle for a second stdio slot.
fn dup(file: &File) -> File {
    file.try_clone().expect("clone fresh log file")
}

fn log_file(path: &Path) -> File {
    File::create(path).unwrap_or_else(|err| {
        eprintln!("warning: cannot create {}: {err}", path.display());
        File::create(std::env::temp_dir().join("dashboard-debug-fallback.log"))
            .expect("temp log file")
    })
}

fn pid_text(pid: Option<u32>) -> String {
    pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
}

fn frame_text(frame: Option<u64>) -> String {
    frame.map(|f| f.to_string()).unwrap_or_else(|| "?".into())
}

/// Socket + auth + `window.list` + one `tree.dump` for the ready frame —
/// the D5 readiness bar, in one attempt.
fn try_bridge_ready() -> Option<(Conn, Option<u64>)> {
    let runtime_dir = dashboard_core::AppPaths::init().runtime_dir().to_path_buf();
    let Ok(token) = std::fs::read_to_string(runtime_dir.join("debug.token")) else {
        return None;
    };
    let token = token.trim().to_string();
    let Ok(stream) = std::os::unix::net::UnixStream::connect(runtime_dir.join("debug.sock")) else {
        return None;
    };
    let mut conn = Conn::new(stream).ok()?;
    conn.auth(&token).ok()?;
    let response = conn.call("window.list", json!({})).ok()?;
    response.get("result")?;
    let dump = conn.call("tree.dump", json!({})).ok()?;
    let frame = dump
        .get("result")
        .and_then(|result| result.get("frame"))
        .and_then(|frame| frame.get("frame_number"))
        .and_then(|number| number.as_u64());
    Some((conn, frame))
}

fn status(name: Option<&str>) -> i32 {
    let root = sessions_root();
    match name {
        Some(name) => {
            if !valid_name(name) {
                eprintln!("invalid session name {name:?}");
                return 2;
            }
            let dirs = SessionDirs::at(&root, name);
            match read_record(&dirs.session_json()) {
                Ok(Some(record)) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&record).expect("serialize record")
                    );
                    println!(
                        "  app alive:    {}",
                        alive_text(record.app_pid, &record.binary, Match::Exact)
                    );
                    println!(
                        "  sway alive:   {}",
                        alive_text(record.sway_pid, "sway", Match::Basename)
                    );
                    let socket = Path::new(&record.state_dir)
                        .join("runtime")
                        .join("debug.sock");
                    println!(
                        "  bridge sock:  {}",
                        if socket.exists() { "present" } else { "gone" }
                    );
                    0
                }
                Ok(None) => {
                    eprintln!("no session {name:?} at {}", dirs.dir.display());
                    1
                }
                Err(err) => {
                    eprintln!("{}: {err}", dirs.session_json().display());
                    1
                }
            }
        }
        None => {
            let mut listed = 0;
            if let Ok(entries) = std::fs::read_dir(&root) {
                for entry in entries.flatten() {
                    let Some(dir_name) = entry.file_name().into_string().ok() else {
                        continue;
                    };
                    if !valid_name(&dir_name) {
                        continue;
                    }
                    let dirs = SessionDirs::at(&root, &dir_name);
                    match read_record(&dirs.session_json()) {
                        Ok(Some(record)) => {
                            listed += 1;
                            println!(
                                "{:<16} app {} · sway {} · frame {}",
                                dir_name,
                                alive_text(record.app_pid, &record.binary, Match::Exact),
                                alive_text(record.sway_pid, "sway", Match::Basename),
                                frame_text(record.frame_at_ready),
                            );
                        }
                        Ok(None) => {
                            listed += 1;
                            println!("{dir_name:<16} no session.json (interrupted start?)");
                        }
                        Err(err) => {
                            listed += 1;
                            println!("{dir_name:<16} corrupt record: {err}");
                        }
                    }
                }
            }
            if listed == 0 {
                println!("no sessions (root: {})", root.display());
            }
            0
        }
    }
}

fn stop(name: &str, keep: bool) -> i32 {
    if !valid_name(name) {
        eprintln!("invalid session name {name:?}");
        return 2;
    }
    let dirs = SessionDirs::at(sessions_root(), name);
    let mut record = match read_record(&dirs.session_json()) {
        Ok(Some(record)) => record,
        Ok(None) => {
            eprintln!(
                "no session record at {} — if the directory is stale, remove it manually",
                dirs.session_json().display()
            );
            return 1;
        }
        Err(err) => {
            eprintln!("{}: {err}", dirs.session_json().display());
            return 1;
        }
    };
    run_stop(
        &mut LiveActor { report: true },
        &mut record,
        &stop_steps(keep),
    );
    if keep {
        println!("session {name} stopped (kept: {})", dirs.dir.display());
    } else {
        println!("session {name} stopped (purged)");
    }
    0
}

/// One rung of the D5/D11 stop ladder, as data — the order is the spec,
/// table-tested below; the actor is the effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopStep {
    /// `app.quit` over the bridge, ACK waited out (D11, graceful-first).
    QuitBridge,
    /// SIGTERM → SIGKILL ladder for the app.
    TerminateApp,
    /// Restore `ScreenReaderEnabled` to the recorded value (D3).
    RestoreFlag,
    TerminateSway,
    TerminateLaunchers,
    Purge,
}

/// The stop ladder (D5/D11): graceful `app.quit` first, then the signal
/// ladder for what is still alive, the flag restore after the app is
/// gone, compositor and launchers last, purge unless `--keep`.
fn stop_steps(keep: bool) -> Vec<StopStep> {
    let mut steps = vec![
        StopStep::QuitBridge,
        StopStep::TerminateApp,
        StopStep::RestoreFlag,
        StopStep::TerminateSway,
        StopStep::TerminateLaunchers,
    ];
    if !keep {
        steps.push(StopStep::Purge);
    }
    steps
}

/// The effects behind [`StopStep`]. The real actor talks to the bridge,
/// signals processes and touches the bus; the test actor records.
trait StopActor {
    /// Ask the app to quit gracefully; `true` when it exited on the ACK.
    fn quit_bridge(&mut self, record: &mut SessionRecord) -> bool;
    fn terminate_app(&mut self, record: &mut SessionRecord);
    fn restore_flag(&mut self, record: &SessionRecord);
    fn terminate_sway(&mut self, record: &mut SessionRecord);
    fn terminate_launchers(&mut self, record: &mut SessionRecord);
    fn purge(&mut self, record: &SessionRecord);
}

fn run_stop<A: StopActor>(actor: &mut A, record: &mut SessionRecord, steps: &[StopStep]) {
    for step in steps {
        match step {
            StopStep::QuitBridge => {
                if actor.quit_bridge(record) {
                    record.app_pid = None;
                }
            }
            StopStep::TerminateApp => actor.terminate_app(record),
            StopStep::RestoreFlag => actor.restore_flag(record),
            StopStep::TerminateSway => actor.terminate_sway(record),
            StopStep::TerminateLaunchers => actor.terminate_launchers(record),
            StopStep::Purge => actor.purge(record),
        }
    }
}

/// The D5/D3/D11 teardown as an actor: only PIDs whose `/proc` cmdline
/// still matches what we spawned are signalled.
struct LiveActor {
    report: bool,
}

impl StopActor for LiveActor {
    fn quit_bridge(&mut self, record: &mut SessionRecord) -> bool {
        quit_app_via_bridge(record, self.report)
    }

    fn terminate_app(&mut self, record: &mut SessionRecord) {
        if let Some(pid) = record.app_pid.take() {
            terminate(pid, &record.binary.clone(), Match::Exact, self.report);
        }
    }

    fn restore_flag(&mut self, record: &SessionRecord) {
        if let Some(flag) = record.screen_reader_at_start {
            if let Err(err) = set_screen_reader_enabled(flag) {
                eprintln!("warning: restoring ScreenReaderEnabled failed: {err}");
            } else if self.report {
                println!("ScreenReaderEnabled restored to {flag}");
            }
        }
    }

    fn terminate_sway(&mut self, record: &mut SessionRecord) {
        if let Some(pid) = record.sway_pid.take() {
            terminate(pid, "sway", Match::Basename, self.report);
        }
    }

    fn terminate_launchers(&mut self, record: &mut SessionRecord) {
        let launchers = std::mem::take(&mut record.a11y_launcher_pids);
        for pid in launchers {
            terminate(pid, "at-spi-bus-launcher", Match::Basename, self.report);
        }
    }

    fn purge(&mut self, record: &SessionRecord) {
        let dir = Path::new(&record.state_dir)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(&record.state_dir));
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            eprintln!("warning: could not remove {}: {err}", dir.display());
        }
    }
}

/// D11 graceful-first: `app.quit` on the session's own bridge, ACK waited
/// out. Any failure (socket gone, auth refused, no reply) is a plain
/// `false` — the SIGTERM ladder still follows, so best-effort is enough.
fn quit_app_via_bridge(record: &SessionRecord, report: bool) -> bool {
    let Some(pid) = record.app_pid else {
        return true;
    };
    if !pid_matches(pid, &record.binary, Match::Exact) {
        // Not ours anymore (pid reuse): never signal, never claim a quit.
        return false;
    }
    let runtime = Path::new(&record.state_dir).join("runtime");
    let Ok(token) = std::fs::read_to_string(runtime.join("debug.token")) else {
        return false;
    };
    let Ok(stream) = std::os::unix::net::UnixStream::connect(runtime.join("debug.sock")) else {
        return false;
    };
    // Bound every read: a hung app must not stretch the stop budget.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let Ok(mut conn) = Conn::new(stream) else {
        return false;
    };
    if conn.auth(token.trim()).is_err() {
        return false;
    }
    let Ok(response) = conn.call("app.quit", json!({})) else {
        return false;
    };
    if response.get("result").is_none() {
        return false;
    }
    let exited = wait_until_gone(pid, Duration::from_secs(5));
    if exited && report {
        println!("pid {pid} exited (app.quit acknowledged)");
    }
    exited
}

fn env(name: &str) -> i32 {
    if !valid_name(name) {
        eprintln!("invalid session name {name:?}");
        return 2;
    }
    let dirs = SessionDirs::at(sessions_root(), name);
    match read_record(&dirs.session_json()) {
        Ok(Some(record)) => {
            let display = record.wayland_display.as_deref().unwrap_or("-");
            println!(
                "DASHBOARD_STATE_DIR={} WAYLAND_DISPLAY={}",
                record.state_dir, display
            );
            0
        }
        Ok(None) => {
            eprintln!("no session {name:?} at {}", dirs.dir.display());
            1
        }
        Err(err) => {
            eprintln!("{}: {err}", dirs.session_json().display());
            1
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- process helpers ------------------------------------------------------

#[derive(Clone, Copy)]
enum Match {
    /// argv[0] equals the recorded spawn path (the app).
    Exact,
    /// the executable basename matches (sway, at-spi-bus-launcher).
    Basename,
}

fn pid_alive(pid: u32) -> bool {
    // Signal 0 probes existence: 0 or EPERM means alive, ESRCH means gone.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn argv0(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
    raw.split('\0')
        .next()
        .filter(|first| !first.is_empty())
        .map(|first| first.to_string())
}

fn pid_matches(pid: u32, expect: &str, mode: Match) -> bool {
    let Some(argv0) = argv0(pid) else {
        return false;
    };
    match mode {
        Match::Exact => argv0 == expect,
        Match::Basename => Path::new(&argv0)
            .file_name()
            .is_some_and(|name| name == expect),
    }
}

fn alive_text(pid: Option<u32>, expect: &str, mode: Match) -> String {
    match pid {
        Some(pid) if pid_matches(pid, expect, mode) => format!("pid {pid} alive"),
        Some(pid) if pid_alive(pid) => format!("pid {pid} alive (does not match {expect:?})"),
        Some(pid) => format!("pid {pid} gone"),
        None => "?".into(),
    }
}

/// TERM, wait out the grace, KILL — signalling only when the PID still
/// looks like ours (D3 kill discipline + pid-reuse guard).
fn terminate(pid: u32, expect: &str, mode: Match, report: bool) {
    if !pid_alive(pid) {
        if report {
            println!("pid {pid} already gone");
        }
        return;
    }
    if !pid_matches(pid, expect, mode) {
        eprintln!(
            "warning: pid {pid} no longer matches {expect:?} — not signalling \
             (pid reuse? inspect /proc/{pid}/cmdline)"
        );
        return;
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if wait_until_gone(pid, Duration::from_secs(3)) {
        if report {
            println!("pid {pid} terminated (SIGTERM)");
        }
        return;
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let killed = wait_until_gone(pid, Duration::from_secs(2));
    if report {
        println!(
            "pid {pid} {}",
            if killed {
                "killed (SIGKILL)"
            } else {
                "did not exit"
            }
        );
    }
}

fn wait_until_gone(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !pid_alive(pid)
}

// ---- wayland socket discovery (D2) ----------------------------------------

fn wayland_sockets(xdg_runtime: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(xdg_runtime) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // wlroots drops a `<socket>.lock` sidecar next to the socket —
            // only the socket itself counts (two entries would read as an
            // ambiguous concurrent start).
            if name.ends_with(".lock") {
                return None;
            }
            name.starts_with("wayland-").then_some(name)
        })
        .collect()
}

fn wait_for_new_socket(
    xdg_runtime: &Path,
    before: &HashSet<String>,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = wayland_sockets(xdg_runtime);
        let new_sockets: Vec<&String> = now.difference(before).collect();
        match new_sockets.as_slice() {
            [one] => return Ok((*one).clone()),
            [] => {}
            many => {
                let names: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
                return Err(format!(
                    "multiple new wayland sockets ({names:?}) — another sway \
                     started concurrently; retry"
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "sway created no wayland socket within {}s (see the session's \
                 sway.log)",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---- session bus (D3): flag record/restore + bus presence -----------------

fn status_proxy() -> Result<atspi::proxy::bus::StatusProxy<'static>, String> {
    zbus::block_on(async {
        let bus = zbus::connection::Connection::session()
            .await
            .map_err(|err| format!("session bus unavailable: {err}"))?;
        atspi::proxy::bus::StatusProxy::new(&bus)
            .await
            .map_err(|err| format!("org.a11y.Status unavailable: {err}"))
    })
}

fn read_screen_reader_enabled() -> Result<bool, String> {
    let step = "reading ScreenReaderEnabled".to_string();
    let proxy = status_proxy().map_err(|err| format!("{step}: {err}"))?;
    zbus::block_on(async {
        proxy
            .screen_reader_enabled()
            .await
            .map_err(|err| format!("{step}: {err}"))
    })
}

fn set_screen_reader_enabled(value: bool) -> Result<(), String> {
    let step = format!("setting ScreenReaderEnabled to {value}");
    let proxy = status_proxy().map_err(|err| format!("{step}: {err}"))?;
    zbus::block_on(async {
        proxy
            .set_screen_reader_enabled(value)
            .await
            .map_err(|err| format!("{step}: {err}"))
    })
}

fn a11y_bus_present() -> Result<bool, String> {
    zbus::block_on(async {
        let bus = zbus::connection::Connection::session()
            .await
            .map_err(|err| format!("session bus unavailable: {err}"))?;
        let dbus = zbus::fdo::DBusProxy::new(&bus)
            .await
            .map_err(|err| format!("DBus proxy unavailable: {err}"))?;
        let name = zbus::names::BusName::try_from("org.a11y.Bus")
            .map_err(|err| format!("bad bus name: {err}"))?;
        dbus.name_has_owner(name)
            .await
            .map_err(|err| format!("NameHasOwner failed: {err}"))
    })
}

/// The dev-shell bus is once-per-shell (AGENTS.md); start one only when
/// absent, and record only the PIDs we spawned (D3).
fn ensure_a11y_bus(dirs: &SessionDirs) -> Result<Vec<u32>, String> {
    if a11y_bus_present().unwrap_or(false) {
        return Ok(Vec::new());
    }
    if find_in_path(
        "at-spi-bus-launcher",
        &std::env::var("PATH").unwrap_or_default(),
    )
    .is_none()
    {
        return Err(
            "at-spi-bus-launcher not found on PATH — enter the dev shell (nix develop)".into(),
        );
    }
    let log = log_file(&dirs.a11y_log());
    let launcher: Child = Command::new("at-spi-bus-launcher")
        .arg("--launch-immediately")
        .stdin(Stdio::null())
        .stdout(dup(&log))
        .stderr(log)
        .spawn()
        .map_err(|err| format!("spawning at-spi-bus-launcher: {err}"))?;
    let pid = launcher.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if a11y_bus_present().unwrap_or(false) {
            return Ok(vec![pid]);
        }
        if !pid_alive(pid) {
            return Err(format!(
                "at-spi-bus-launcher exited before org.a11y.Bus appeared (see {})",
                dirs.a11y_log().display()
            ));
        }
        if Instant::now() >= deadline {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            return Err("org.a11y.Bus did not appear within 5s of launching".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    /// Records the ladder as it runs — the ordering spec, table-tested.
    struct FakeActor {
        calls: Vec<&'static str>,
        quit_succeeds: bool,
    }

    impl StopActor for FakeActor {
        fn quit_bridge(&mut self, _record: &mut SessionRecord) -> bool {
            self.calls.push("quit");
            self.quit_succeeds
        }

        fn terminate_app(&mut self, record: &mut SessionRecord) {
            // Mirror the real actor: a rung whose PID is already gone
            // (quit acknowledged) is a no-op, not a call.
            if record.app_pid.take().is_some() {
                self.calls.push("term-app");
            }
        }

        fn restore_flag(&mut self, _record: &SessionRecord) {
            self.calls.push("flag");
        }

        fn terminate_sway(&mut self, record: &mut SessionRecord) {
            self.calls.push("term-sway");
            record.sway_pid = None;
        }

        fn terminate_launchers(&mut self, record: &mut SessionRecord) {
            self.calls.push("term-launchers");
            record.a11y_launcher_pids.clear();
        }

        fn purge(&mut self, _record: &SessionRecord) {
            self.calls.push("purge");
        }
    }

    fn record() -> SessionRecord {
        SessionRecord {
            name: "t".into(),
            created_at: 0,
            binary: "/bin/app".into(),
            size: (800, 600),
            app_pid: Some(11),
            sway_pid: Some(22),
            a11y_launcher_pids: vec![33],
            wayland_display: Some("wayland-9".into()),
            state_dir: "/sessions/t/state".into(),
            screen_reader_at_start: Some(false),
            frame_at_ready: None,
        }
    }

    #[test]
    fn stop_ladder_orders_graceful_first_and_purges_by_default() {
        assert_eq!(
            stop_steps(false),
            vec![
                StopStep::QuitBridge,
                StopStep::TerminateApp,
                StopStep::RestoreFlag,
                StopStep::TerminateSway,
                StopStep::TerminateLaunchers,
                StopStep::Purge,
            ]
        );
        assert_eq!(stop_steps(true).last(), Some(&StopStep::TerminateLaunchers));
    }

    #[test]
    fn acknowledged_quit_skips_the_app_signal_ladder() {
        let mut actor = FakeActor {
            calls: Vec::new(),
            quit_succeeds: true,
        };
        let mut rec = record();
        run_stop(&mut actor, &mut rec, &stop_steps(false));
        assert_eq!(
            actor.calls,
            vec!["quit", "flag", "term-sway", "term-launchers", "purge"]
        );
        assert_eq!(rec.app_pid, None);
    }

    #[test]
    fn failed_quit_falls_through_to_sigterm() {
        let mut actor = FakeActor {
            calls: Vec::new(),
            quit_succeeds: false,
        };
        let mut rec = record();
        run_stop(&mut actor, &mut rec, &stop_steps(true));
        assert_eq!(
            actor.calls,
            vec!["quit", "term-app", "flag", "term-sway", "term-launchers"]
        );
    }
}
