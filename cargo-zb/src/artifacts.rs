use std::path::{Path, PathBuf};

use anyhow::Result;
use crate::cache::CacheBackend;
use crate::hash::CacheKey;

pub fn store(
    cache: &dyn CacheBackend,
    build_key: &CacheKey,
    paths: &[PathBuf],
    target_dir: &Path,
) -> Result<usize> {
    let mut manifest: Vec<String> = Vec::new();

    for path in paths {
        if !path.exists() {
            tracing::warn!("artifact not found, skipping: {}", path.display());
            continue;
        }
        let rel = path.strip_prefix(target_dir).unwrap_or(path);
        let rel_str = rel.to_string_lossy().to_string();

        cache.store_artifact_from_file(build_key.as_bytes(), &rel_str, path)?;
        manifest.push(rel_str);
    }

    cache.finalize_build(build_key.as_bytes(), &manifest)?;
    Ok(manifest.len())
}

pub fn restore(
    cache: &dyn CacheBackend,
    build_key: &CacheKey,
    target_dir: &Path,
    n_threads: usize,
) -> Result<usize> {
    let manifest = cache.list_artifacts(build_key.as_bytes())?;
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

    let n_threads = n_threads.max(1);
    let chunk_size = (manifest.len() + n_threads - 1) / n_threads;
    let errors: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let restored = std::sync::atomic::AtomicUsize::new(0);

    let errors = &errors;
    let restored = &restored;
    std::thread::scope(|s| {
        for chunk in manifest.chunks(chunk_size) {
            s.spawn(move || {
                for rel_str in chunk {
                    let dest = target_dir.join(rel_str);
                    let written = cache.restore_artifact(
                        build_key.as_bytes(),
                        rel_str,
                        &dest,
                    );
                    match written {
                        Ok(true) => {
                            restored.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        Ok(false) => {
                            tracing::warn!("missing cached file: {}", rel_str);
                        }
                        Err(e) => {
                            errors.lock().unwrap().push(format!("{}: {}", rel_str, e));
                        }
                    }
                }
            });
        }
    });

    let errs = errors.lock().unwrap();
    if !errs.is_empty() {
        tracing::warn!("{} files failed to restore", errs.len());
    }

    Ok(restored.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn snapshot_target_dir(target_dir: &Path) -> std::collections::HashMap<PathBuf, std::time::SystemTime> {
    let mut snapshot = std::collections::HashMap::new();
    if !target_dir.exists() {
        return snapshot;
    }
    for entry in walkdir::WalkDir::new(target_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    snapshot.insert(entry.into_path(), mtime);
                }
            }
        }
    }
    snapshot
}

pub fn diff_target_dir(
    target_dir: &Path,
    before: &std::collections::HashMap<PathBuf, std::time::SystemTime>,
) -> Vec<PathBuf> {
    let mut new_files = Vec::new();
    if !target_dir.exists() {
        return new_files;
    }
    for entry in walkdir::WalkDir::new(target_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let path = entry.into_path();
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    match before.get(&path) {
                        None => new_files.push(path),
                        Some(old_mtime) if mtime > *old_mtime => new_files.push(path),
                        _ => {}
                    }
                }
            }
        }
    }
    new_files.sort();
    new_files
}
