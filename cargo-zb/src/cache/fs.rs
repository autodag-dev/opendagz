use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::CacheBackend;

pub struct FsCache {
    root: PathBuf,
}

impl FsCache {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating cache dir {}", root.display()))?;
        Ok(Self { root })
    }

    fn build_dir(&self, build_key: &[u8; 32]) -> PathBuf {
        self.root.join(super::hex(build_key))
    }

    fn manifest_path(&self, build_key: &[u8; 32]) -> PathBuf {
        self.build_dir(build_key).join("manifest.json")
    }

    fn artifact_path(&self, build_key: &[u8; 32], rel_path: &str) -> PathBuf {
        self.build_dir(build_key).join("artifacts").join(rel_path)
    }
}

impl CacheBackend for FsCache {
    fn contains_build(&self, build_key: &[u8; 32]) -> Result<bool> {
        Ok(self.manifest_path(build_key).exists())
    }

    fn list_artifacts(&self, build_key: &[u8; 32]) -> Result<Vec<String>> {
        let path = self.manifest_path(build_key);
        match std::fs::read(&path) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("reading manifest {}", path.display())),
        }
    }

    fn get_artifact(&self, build_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>> {
        let path = self.artifact_path(build_key, rel_path);
        match std::fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading cached artifact {}", path.display())),
        }
    }

    fn put_artifact(&self, build_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()> {
        let path = self.artifact_path(build_key, rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, data)
            .with_context(|| format!("writing artifact {}", path.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming artifact {}", path.display()))?;
        Ok(())
    }

    fn finalize_build(&self, build_key: &[u8; 32], artifacts: &[String]) -> Result<()> {
        let path = self.manifest_path(build_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec(artifacts)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn store_artifact_from_file(
        &self,
        build_key: &[u8; 32],
        rel_path: &str,
        src: &Path,
    ) -> Result<()> {
        let dest = self.artifact_path(build_key, rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("tmp");
        copy_file_range_or_fallback(src, &tmp)?;
        std::fs::rename(&tmp, &dest)
            .with_context(|| format!("renaming artifact {}", dest.display()))?;
        Ok(())
    }

    fn restore_artifact(
        &self,
        build_key: &[u8; 32],
        rel_path: &str,
        dest: &Path,
    ) -> Result<bool> {
        let src_path = self.artifact_path(build_key, rel_path);
        match copy_file_range_or_fallback(&src_path, dest) {
            Ok(()) => Ok(true),
            Err(e) if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn name(&self) -> &str {
        "fs"
    }
}

fn copy_file_range_or_fallback(src: &Path, dest: &Path) -> Result<()> {
    let src_file = File::open(src)
        .with_context(|| format!("opening {}", src.display()))?;
    let len = src_file.metadata()?.len() as usize;
    let dst_file = File::create(dest)
        .with_context(|| format!("creating {}", dest.display()))?;

    let mut copied = 0usize;
    while copied < len {
        let n = unsafe {
            libc::copy_file_range(
                src_file.as_raw_fd(),
                std::ptr::null_mut(),
                dst_file.as_raw_fd(),
                std::ptr::null_mut(),
                len - copied,
                0,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOSYS)
                || err.raw_os_error() == Some(libc::EXDEV)
            {
                drop(src_file);
                drop(dst_file);
                std::fs::copy(src, dest)?;
                return Ok(());
            }
            return Err(err).with_context(|| format!("copy_file_range {}", dest.display()));
        }
        if n == 0 {
            break;
        }
        copied += n as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path()).unwrap();
        let key = blake3::hash(b"test-build");
        let key_bytes = key.as_bytes();

        assert!(!cache.contains_build(key_bytes).unwrap());
        assert!(cache.list_artifacts(key_bytes).unwrap().is_empty());

        cache.put_artifact(key_bytes, "debug/libfoo.rlib", b"rlib data").unwrap();
        cache.put_artifact(key_bytes, "debug/foo", b"binary data").unwrap();
        cache.finalize_build(key_bytes, &[
            "debug/libfoo.rlib".into(),
            "debug/foo".into(),
        ]).unwrap();

        assert!(cache.contains_build(key_bytes).unwrap());
        let artifacts = cache.list_artifacts(key_bytes).unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            cache.get_artifact(key_bytes, "debug/libfoo.rlib").unwrap().unwrap(),
            b"rlib data"
        );
        assert_eq!(
            cache.get_artifact(key_bytes, "debug/foo").unwrap().unwrap(),
            b"binary data"
        );
    }
}
