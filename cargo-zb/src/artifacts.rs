//! Per-unit artifact attribution and cache storage.
//!
//! After cargo finishes a build, the files in `target/` need to be sorted
//! into per-unit bundles so each unit can be cached / restored independently.
//! Cargo names every output with the unit's `c_extra_filename` hash:
//!
//! - `target/<target>/release/.fingerprint/<crate>-<hash>/...`
//! - `target/<target>/release/deps/<crate>-<hash>.{rlib,rmeta,so,d}`
//! - `target/<target>/release/build/<pkg>-<hash>/...`  (build script run + COMPILE)
//! - `target/<target>/release/<bin_name>` — *no hash*, link to the bin.
//!
//! We use the per-unit directory paths cargo's `CompilationFiles` exposes
//! (`fingerprint_dir`, `build_script_run_dir`, `build_script_dir`) plus a
//! deps-dir filter on `c_extra_filename`. The naked binary in the build dir
//! root is attributed to the bin unit by name (cargo's `Compilation::binaries`
//! / `cdylibs` give the post-build paths if needed).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo::core::compiler::{BuildRunner, CompileMode, Unit};

use crate::cache::CacheBackend;

/// Files belonging to one unit that we want to cache + restore.
#[derive(Debug, Default, Clone)]
pub struct UnitArtifacts {
    /// Absolute paths of files to bundle into the unit's cache entry.
    pub files: Vec<PathBuf>,
}

/// Enumerate every file currently on disk that belongs to `unit`.
pub fn collect_unit_artifacts(
    runner: &BuildRunner<'_, '_>,
    unit: &Unit,
) -> UnitArtifacts {
    let files = runner.files();
    let metadata = files.metadata(unit);
    let unit_hash = metadata.c_extra_filename().map(|h| h.to_string());

    let mut paths = Vec::new();

    // Per-unit fingerprint directory: everything in it.
    let fp_dir = files.fingerprint_dir(unit);
    walk_dir_files(&fp_dir, &mut paths);

    if unit.mode == CompileMode::RunCustomBuild {
        // Build-script-run unit: the entire build/<pkg>-<hash>/ tree, including
        // OUT_DIR, the parsed `output` file, invoked.timestamp, etc.
        let run_dir = files.build_script_run_dir(unit);
        walk_dir_files(&run_dir, &mut paths);
    } else if unit.target.is_custom_build() {
        // Compile-of-build.rs unit (mode == Build for a custom-build target):
        // the build-script binary lives in build_script_dir.
        let bs_dir = files.build_script_dir(unit);
        walk_dir_files(&bs_dir, &mut paths);
    }

    // Deps dir entries matching this unit's hash. cargo emits multiple files
    // here per unit: <prefix><crate>-<hash>.{rlib,rmeta,so,dylib,d,o}.
    if let Some(hash) = &unit_hash {
        let deps_dir = files.deps_dir(unit);
        if let Ok(entries) = std::fs::read_dir(&deps_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else { continue };
                if name_str.contains(hash) {
                    paths.push(entry.path());
                }
            }
        }
    }

    paths.sort();
    paths.dedup();
    UnitArtifacts { files: paths }
}

fn walk_dir_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            out.push(entry.into_path());
        }
    }
}

/// Store a unit's artifacts under `unit_key`. Each file's path is stored as
/// relative to `target_dir` (so restore can reconstruct under any target dir).
pub fn store_unit(
    cache: &dyn CacheBackend,
    unit_key: &[u8; 32],
    artifacts: &UnitArtifacts,
    target_dir: &Path,
) -> Result<usize> {
    let mut manifest: Vec<String> = Vec::new();
    for path in &artifacts.files {
        if !path.exists() {
            tracing::warn!("artifact not found, skipping: {}", path.display());
            continue;
        }
        let rel = path.strip_prefix(target_dir).unwrap_or(path);
        let rel_str = rel.to_string_lossy().to_string();
        cache
            .store_artifact_from_file(unit_key, &rel_str, path)
            .with_context(|| format!("storing {}", path.display()))?;
        manifest.push(rel_str);
    }
    cache.finalize_unit(unit_key, &manifest)?;
    Ok(manifest.len())
}

/// Restore a unit's artifacts from cache into `target_dir`.
pub fn restore_unit(
    cache: &dyn CacheBackend,
    unit_key: &[u8; 32],
    target_dir: &Path,
) -> Result<usize> {
    let manifest = cache.list_artifacts(unit_key)?;
    if manifest.is_empty() {
        return Ok(0);
    }
    let mut dirs_seen = std::collections::HashSet::new();
    for rel_str in &manifest {
        let dest = target_dir.join(rel_str);
        if let Some(parent) = dest.parent() {
            if dirs_seen.insert(parent.to_path_buf()) {
                std::fs::create_dir_all(parent)?;
            }
        }
    }
    let mut restored = 0;
    for rel_str in &manifest {
        let dest = target_dir.join(rel_str);
        match cache.restore_artifact(unit_key, rel_str, &dest)? {
            true => restored += 1,
            false => tracing::warn!("missing cached file: {}", rel_str),
        }
    }
    Ok(restored)
}
