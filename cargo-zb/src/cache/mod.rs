pub mod fs;
pub mod lmdb;
#[cfg(feature = "tikv")]
pub mod tikv;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

pub trait CacheBackend: Send + Sync {
    fn contains_build(&self, build_key: &[u8; 32]) -> Result<bool>;
    fn list_artifacts(&self, build_key: &[u8; 32]) -> Result<Vec<String>>;
    fn get_artifact(&self, build_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>>;

    fn restore_artifact(
        &self,
        build_key: &[u8; 32],
        rel_path: &str,
        dest: &Path,
    ) -> Result<bool> {
        match self.get_artifact(build_key, rel_path)? {
            Some(data) => {
                std::fs::write(dest, &data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn put_artifact(&self, build_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()>;

    fn store_artifact_from_file(
        &self,
        build_key: &[u8; 32],
        rel_path: &str,
        src: &Path,
    ) -> Result<()> {
        let data = std::fs::read(src)?;
        self.put_artifact(build_key, rel_path, &data)
    }

    fn finalize_build(&self, build_key: &[u8; 32], artifacts: &[String]) -> Result<()>;
    fn name(&self) -> &str;
}
