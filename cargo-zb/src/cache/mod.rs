pub mod fs;
pub mod lmdb;
#[cfg(feature = "tikv")]
pub mod tikv;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub fn default_cache_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_ZB_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(Path::new(&home).join(".cache").join("cargo-zb"))
}

pub fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Files and env vars whose state must be folded into a unit's cache key.
///
/// Each entry carries a snapshot of its content/value at the time the manifest
/// was last finalized, so a cache miss can pinpoint which input changed
/// (`diff_current`). The per-entry snapshots do NOT participate in `shape_hash`
/// (which keys the manifest itself), only in `content_hash` and `diff_current`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DynamicInputs {
    pub paths: Vec<DynPath>,
    pub envs: Vec<DynEnv>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynPath {
    pub path: PathBuf,
    /// blake3 of the file/dir contents at the time this manifest was written.
    /// `[0; 32]` is the sentinel for "missing" (the file didn't exist).
    pub stored_hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynEnv {
    pub name: String,
    pub stored_value: Option<String>,
}

/// What changed between a stored manifest's snapshot and current filesystem/env state.
#[derive(Debug, Clone, Default)]
pub struct DiffReport {
    pub changed_paths: Vec<PathBuf>,
    pub missing_paths: Vec<PathBuf>,
    pub appeared_paths: Vec<PathBuf>,
    pub changed_envs: Vec<String>,
}

impl DiffReport {
    pub fn is_empty(&self) -> bool {
        self.changed_paths.is_empty()
            && self.missing_paths.is_empty()
            && self.appeared_paths.is_empty()
            && self.changed_envs.is_empty()
    }

    pub fn total(&self) -> usize {
        self.changed_paths.len()
            + self.missing_paths.len()
            + self.appeared_paths.len()
            + self.changed_envs.len()
    }
}

impl DynamicInputs {
    /// Stable identity hash of declarations only — used to dedup manifests
    /// against the same static_key when build scripts emit identical
    /// `rerun-if-*` lists across runs.
    pub fn shape_hash(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"dyn-inputs-shape-v2\0");
        let mut paths: Vec<&PathBuf> = self.paths.iter().map(|p| &p.path).collect();
        paths.sort();
        for p in &paths {
            hasher.update(p.to_string_lossy().as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"paths-end\0");
        let mut envs: Vec<&String> = self.envs.iter().map(|e| &e.name).collect();
        envs.sort();
        for k in &envs {
            hasher.update(k.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"envs-end\0");
        *hasher.finalize().as_bytes()
    }

    /// Hash of *current* contents of declared files + *current* values of
    /// declared env vars. Combined with a unit's static_key + dep full_keys
    /// to derive the unit's full content-addressed cache key. Independent of
    /// `stored_hash`/`stored_value` snapshots.
    pub fn content_hash<F: Fn(&str) -> Option<String>>(&self, env_lookup: F) -> Result<[u8; 32]> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"dyn-inputs-content-v2\0");

        let mut paths: Vec<&DynPath> = self.paths.iter().collect();
        paths.sort_by(|a, b| a.path.cmp(&b.path));
        for p in &paths {
            let h = hash_path_current(&p.path)?;
            hasher.update(p.path.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            hasher.update(&h);
        }
        hasher.update(b"paths-end\0");

        let mut envs: Vec<&DynEnv> = self.envs.iter().collect();
        envs.sort_by(|a, b| a.name.cmp(&b.name));
        for e in &envs {
            hasher.update(e.name.as_bytes());
            hasher.update(b"=");
            match env_lookup(&e.name) {
                Some(v) => {
                    hasher.update(b"s");
                    hasher.update(&(v.len() as u64).to_le_bytes());
                    hasher.update(v.as_bytes());
                }
                None => {
                    hasher.update(b"-");
                }
            }
            hasher.update(b"\0");
        }
        hasher.update(b"envs-end\0");

        Ok(*hasher.finalize().as_bytes())
    }

    /// Compare current state against this manifest's stored snapshot.
    pub fn diff_current<F: Fn(&str) -> Option<String>>(&self, env_lookup: F) -> Result<DiffReport> {
        let mut report = DiffReport::default();
        for p in &self.paths {
            let cur = hash_path_current(&p.path)?;
            let stored_missing = p.stored_hash == [0u8; 32];
            let cur_missing = cur == [0u8; 32];
            match (stored_missing, cur_missing) {
                (true, true) => {}
                (true, false) => report.appeared_paths.push(p.path.clone()),
                (false, true) => report.missing_paths.push(p.path.clone()),
                (false, false) if cur != p.stored_hash => report.changed_paths.push(p.path.clone()),
                _ => {}
            }
        }
        for e in &self.envs {
            let cur = env_lookup(&e.name);
            if cur.as_deref() != e.stored_value.as_deref() {
                report.changed_envs.push(e.name.clone());
            }
        }
        Ok(report)
    }
}

/// Hash content of a single path (file or directory). Returns `[0; 32]` if
/// the path does not exist — used as a stable sentinel.
pub fn hash_path_current(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok([0u8; 32]),
        Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
    };
    if meta.is_file() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("hashing {}", path.display()))?;
        hasher.update(b"f");
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    } else if meta.is_dir() {
        hasher.update(b"d");
        let mut entries: Vec<_> = walkdir::WalkDir::new(path)
            .sort_by_file_name()
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .collect();
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        for entry in entries {
            let rel = entry.path().strip_prefix(path).unwrap_or(entry.path());
            hasher.update(rel.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            let bytes = std::fs::read(entry.path())
                .with_context(|| format!("hashing {}", entry.path().display()))?;
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
    } else {
        // non-regular (symlink to nowhere, socket, etc.) — treat as missing
        return Ok([0u8; 32]);
    }
    Ok(*hasher.finalize().as_bytes())
}

pub trait CacheBackend: Send + Sync {
    fn contains_unit(&self, unit_key: &[u8; 32]) -> Result<bool>;

    fn list_artifacts(&self, unit_key: &[u8; 32]) -> Result<Vec<String>>;

    fn get_artifact(&self, unit_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>>;

    fn restore_artifact(
        &self,
        unit_key: &[u8; 32],
        rel_path: &str,
        dest: &Path,
    ) -> Result<bool> {
        match self.get_artifact(unit_key, rel_path)? {
            Some(data) => {
                std::fs::write(dest, &data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn put_artifact(&self, unit_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()>;

    fn store_artifact_from_file(
        &self,
        unit_key: &[u8; 32],
        rel_path: &str,
        src: &Path,
    ) -> Result<()> {
        let data = std::fs::read(src)?;
        self.put_artifact(unit_key, rel_path, &data)
    }

    fn finalize_unit(&self, unit_key: &[u8; 32], artifacts: &[String]) -> Result<()>;

    fn list_dynamic_inputs(&self, static_key: &[u8; 32]) -> Result<Vec<DynamicInputs>>;

    /// Record a dynamic-inputs manifest. Overwrites any existing entry with
    /// the same `shape_hash` so per-entry snapshots stay current.
    fn put_dynamic_inputs(&self, static_key: &[u8; 32], inputs: &DynamicInputs) -> Result<()>;

    fn name(&self) -> &str;
}
