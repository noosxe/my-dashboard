//! Fixture generations (docs/0084 §8): one shared counter that a receipt
//! advances with `fixture bump`, and the lookup that turns it into a
//! different answer.
//!
//! A fixture `<name>.json` may have siblings `<name>.g<N>.json`. At
//! generation G the transport serves the sibling with the **highest N ≤
//! G**, and falls back to `<name>.json` when there is none (always at
//! generation 0). So a fixture set with no `.g<N>` files behaves exactly
//! as before, and a generation only has to name the files that change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The shared counter. Starts at 0; only `fixture bump` moves it.
#[derive(Debug, Default)]
pub(crate) struct Generation(AtomicU64);

impl Generation {
    pub(crate) fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Advance by one; or jump to `to` (any value, forwards or back, so
    /// a receipt can return to an earlier state). Returns the new value.
    pub(crate) fn bump(&self, to: Option<u64>) -> u64 {
        match to {
            Some(n) => {
                self.0.store(n, Ordering::SeqCst);
                n
            }
            None => self.0.fetch_add(1, Ordering::SeqCst) + 1,
        }
    }
}

/// The file that answers `name` (`PrDetail.101.json`) under `dir` at
/// generation `current`: the highest `<stem>.g<N>.json` with N ≤ `current` and
/// N ≥ 1, else `name` itself when that exists, else `None`.
pub(crate) fn resolve(dir: &Path, name: &str, current: u64) -> Option<PathBuf> {
    let stem = name.strip_suffix(".json")?;
    let best = if current == 0 {
        None
    } else {
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|f| generation_of(&f, stem))
            .filter(|n| (1..=current).contains(n))
            .max()
    };
    let chosen = match best {
        Some(n) => dir.join(format!("{stem}.g{n}.json")),
        None => dir.join(name),
    };
    chosen.is_file().then_some(chosen)
}

/// `PrDetail.101.g3.json` with stem `PrDetail.101` → `Some(3)`.
fn generation_of(file: &str, stem: &str) -> Option<u64> {
    let n = file
        .strip_prefix(stem)?
        .strip_prefix(".g")?
        .strip_suffix(".json")?;
    // `g01` and `g+1` are not generation names.
    (!n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str, files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gh-gen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), "{}").unwrap();
        }
        d
    }

    fn name(p: Option<PathBuf>) -> Option<String> {
        p.map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
    }

    #[test]
    fn highest_generation_at_or_below_current_wins() {
        let d = dir("pick", &["A.json", "A.g1.json", "A.g3.json", "A.g10.json"]);
        assert_eq!(name(resolve(&d, "A.json", 0)).as_deref(), Some("A.json"));
        assert_eq!(name(resolve(&d, "A.json", 1)).as_deref(), Some("A.g1.json"));
        assert_eq!(name(resolve(&d, "A.json", 2)).as_deref(), Some("A.g1.json"));
        assert_eq!(name(resolve(&d, "A.json", 3)).as_deref(), Some("A.g3.json"));
        assert_eq!(name(resolve(&d, "A.json", 9)).as_deref(), Some("A.g3.json"));
        assert_eq!(
            name(resolve(&d, "A.json", 10)).as_deref(),
            Some("A.g10.json")
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn falls_back_to_the_plain_name_and_to_nothing() {
        let d = dir("fallback", &["B.json", "C.g2.json", "B.x.g1.json"]);
        // No generation file for B: today's name at any generation.
        assert_eq!(name(resolve(&d, "B.json", 5)).as_deref(), Some("B.json"));
        // A generation-only fixture has no answer before its generation.
        assert_eq!(resolve(&d, "C.json", 1), None);
        assert_eq!(name(resolve(&d, "C.json", 2)).as_deref(), Some("C.g2.json"));
        // A selector's own file isn't another name's generation.
        assert_eq!(name(resolve(&d, "B.json", 1)).as_deref(), Some("B.json"));
        assert_eq!(resolve(&d, "Missing.json", 1), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn malformed_generation_names_are_ignored() {
        let d = dir(
            "bad",
            &["D.json", "D.g0.json", "D.g01.json", "D.gx.json", "D.g.json"],
        );
        assert_eq!(name(resolve(&d, "D.json", 4)).as_deref(), Some("D.json"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn bump_advances_or_jumps() {
        let g = Generation::default();
        assert_eq!(g.current(), 0);
        assert_eq!(g.bump(None), 1);
        assert_eq!(g.bump(None), 2);
        assert_eq!(g.bump(Some(7)), 7);
        assert_eq!(g.bump(Some(0)), 0);
        assert_eq!(g.current(), 0);
    }
}
