//! Sample banks: folders under the sample root named like `bd/`, `hh/`, … with `.wav` files.
//! References use **1-based** indexing (Tidal-style `bd:3` = third file in sorted order).

use std::path::{Path, PathBuf};

/// List `.wav` files in `sample_root/<bank>/`, sorted lexicographically by filename (Unicode).
pub fn list_bank_wavs(sample_root: &Path, bank: &str) -> Result<Vec<PathBuf>, String> {
    let dir = sample_root.join(bank);
    let rd =
        std::fs::read_dir(&dir).map_err(|e| format!("bank `{bank}` ({}): {e}", dir.display()))?;

    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("wav"))
                .unwrap_or(false)
        })
        .collect();

    paths.sort_by(|a, b| {
        let fa = a
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fb = b
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        fa.cmp(&fb)
    });

    Ok(paths)
}

/// Resolve `bank:index` with **1-based** index (e.g. `3` → third file).
pub fn resolve_bank_sample(
    sample_root: &Path,
    bank: &str,
    index_1based: u32,
) -> Result<PathBuf, String> {
    if index_1based == 0 {
        return Err("bank index must be >= 1 (1-based)".into());
    }
    let files = list_bank_wavs(sample_root, bank)?;
    if files.is_empty() {
        return Err(format!(
            "bank `{bank}` has no .wav files under {}",
            sample_root.join(bank).display()
        ));
    }
    let i = (index_1based as usize).saturating_sub(1);
    if i >= files.len() {
        return Err(format!(
            "bank `{bank}`: index {index_1based} out of range (1–{})",
            files.len()
        ));
    }
    Ok(files[i].clone())
}
