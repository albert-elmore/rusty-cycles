//! Phase 0: stable, comparable representation of scheduled hits for tests and future goldens.
//!
//! See repository file `docs/phase0.md` for scope and non-goals.

use std::path::{Path, PathBuf};

/// One resolved hit in `[0, 1)` of a cycle, comparable across platforms.
#[derive(Debug, Clone)]
pub struct NormalizedHit {
    /// Onset within the cycle, **relative** to `sample_root` when possible (POSIX separators).
    pub phase: f64,
    pub sample_relpath: String,
    pub gain: f32,
    pub pan: f32,
}

impl PartialEq for NormalizedHit {
    fn eq(&self, other: &Self) -> bool {
        (self.phase - other.phase).abs() < 1e-8
            && self.sample_relpath == other.sample_relpath
            && (self.gain - other.gain).abs() < 1e-5
            && (self.pan - other.pan).abs() < 1e-5
    }
}

impl Eq for NormalizedHit {}

/// Turn scheduler output into sorted, root-relative paths for regression tests.
pub fn normalize_schedule(
    sample_root: &Path,
    events: Vec<(
        f64,
        PathBuf,
        f32,
        f32,
        Option<f32>,
        Option<f32>,
        Option<i32>,
        Option<u32>,
        Option<f32>,
    )>,
) -> Vec<NormalizedHit> {
    let root_ok = sample_root.canonicalize().ok();
    let mut out = Vec::with_capacity(events.len());
    for (phase, path, gain, pan, _, _, _, _, _) in events {
        let sample_relpath = match &root_ok {
            Some(root) => match path.canonicalize() {
                Ok(can) => can
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| fallback_sample_id(&path)),
                Err(_) => fallback_sample_id(&path),
            },
            None => fallback_sample_id(&path),
        };
        out.push(NormalizedHit {
            phase,
            sample_relpath,
            gain,
            pan,
        });
    }
    out.sort_by(|a, b| {
        a.phase
            .partial_cmp(&b.phase)
            .unwrap()
            .then_with(|| a.sample_relpath.cmp(&b.sample_relpath))
            .then_with(|| a.gain.partial_cmp(&b.gain).unwrap())
            .then_with(|| a.pan.partial_cmp(&b.pan).unwrap())
    });
    out
}

fn fallback_sample_id(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
