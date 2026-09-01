//! Logging init (docs/0008): flexi_logger behind the `log` facade.
//!
//! One writer setup, no async: a rotating file under [`AppPaths::logs_dir`]
//! (1 MiB per file, 5 rotated files kept → ≈6 MiB hard cap) plus an
//! error/warn/info duplicate to stderr; debug/trace are file-only. The spec
//! comes from `RUST_LOG` when set, else the built-in default — and logging
//! misconfiguration never blocks boot (fallback ladder: file logger →
//! stderr-only → run unlogged; docs/0008 D5).
//!
//! Emission rules live in docs/0008 §5 (event catalog) and D6/D7: no macros
//! in render code, no token material, no device-flow user codes, no
//! PR/issue titles or notification subjects — identifiers and counts only.

use flexi_logger::{Cleanup, Criterion, Duplicate, FileSpec, Logger, Naming};

use crate::AppPaths;

/// Built-in default spec (docs/0008 D3): global floor `warn` — gpui's
/// records land without its debug noise flooding the file — and our three
/// crates at `info`. Target prefixes match crate-qualified module targets
/// (`dashboard_module_github::service` etc.).
pub const DEFAULT_SPEC: &str =
    "warn,dashboard_app=info,dashboard_core=info,dashboard_module_github=info";

/// Log file basename (docs/0008 D2). With `Naming::Numbers` rotation the
/// live file is `dashboard_rCURRENT.log` and rotated files
/// `dashboard_r00000.log`…`dashboard_r00004.log` (rotation replaces the
/// timestamp infix with a serial number).
const BASENAME: &str = "dashboard";
const ROTATE_BYTES: u64 = 1024 * 1024;
const KEEP_FILES: usize = 5;

/// The spec string that wins: `RUST_LOG` when set and non-empty, else the
/// built-in default. A malformed value is detected later by
/// [`Logger::try_with_str`] and falls back to the default there.
fn resolve_spec(env: Option<String>) -> String {
    env.filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_SPEC.to_string())
}

/// Start the app logger. Called once from `main` right after
/// [`AppPaths::init`] and before anything else (docs/0008 D5). The returned
/// handle must be kept alive for the process lifetime — dropping it flushes
/// and shuts the writers down mid-run; `main` leaks it deliberately.
pub fn init(paths: &AppPaths) -> Option<flexi_logger::LoggerHandle> {
    let spec = resolve_spec(std::env::var("RUST_LOG").ok());
    let logger = match Logger::try_with_str(&spec) {
        Ok(logger) => logger,
        Err(err) => {
            // Malformed RUST_LOG (docs/0008 D3): note + built-in default.
            eprintln!(
                "dashboard: RUST_LOG={spec:?} failed to parse ({err}); using {DEFAULT_SPEC:?}"
            );
            Logger::try_with_str(DEFAULT_SPEC).ok()?
        }
    };

    let started = logger
        .log_to_file(
            FileSpec::default()
                .directory(paths.logs_dir())
                .basename(BASENAME),
        )
        .rotate(
            Criterion::Size(ROTATE_BYTES),
            Naming::Numbers,
            Cleanup::KeepLogFiles(KEEP_FILES),
        )
        .duplicate_to_stderr(Duplicate::Info)
        // Default `WriteMode::Direct`: unbuffered (crash-safe, real-time,
        // no extra thread) — docs/0008 D2.
        .start();

    match started {
        Ok(handle) => {
            log::info!(
                target: "dashboard_app",
                "logging started: file={} (rotate {}B, keep {})",
                paths.logs_dir().join(format!("{BASENAME}_rCURRENT.log")).display(),
                ROTATE_BYTES,
                KEEP_FILES,
            );
            Some(handle)
        }
        Err(err) => {
            // Fallback ladder (docs/0008 D5): file logging failed →
            // stderr-only; that fails too → run unlogged. Never fatal.
            eprintln!("dashboard: file logging failed ({err}); falling back to stderr-only");
            match Logger::try_with_str(&spec)
                .or_else(|_| Logger::try_with_str(DEFAULT_SPEC))
                .map(|l| l.log_to_stderr().start())
            {
                Ok(Ok(handle)) => {
                    log::warn!(target: "dashboard_app", "running with stderr-only logging");
                    Some(handle)
                }
                _ => {
                    eprintln!("dashboard: logging unavailable; continuing without logs");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spec_string_parses() {
        // The constant is load-bearing: a typo here would silently log
        // nothing at boot (spec parse failure → fallback note on stderr).
        assert!(Logger::try_with_str(DEFAULT_SPEC).is_ok());
        assert!(Logger::try_with_str("bogus====").is_err());
    }

    #[test]
    fn rust_log_set_wins_and_unset_falls_back_to_default() {
        assert_eq!(
            resolve_spec(Some("debug".into())),
            "debug",
            "RUST_LOG replaces the whole spec (docs/0008 D3)"
        );
        assert_eq!(resolve_spec(Some(String::new()),), DEFAULT_SPEC);
        assert_eq!(resolve_spec(None), DEFAULT_SPEC);
    }
}
