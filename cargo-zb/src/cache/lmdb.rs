use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use heed::types::Bytes;
use heed::{Database, EnvOpenOptions};

use super::{CacheBackend, DynamicInputs};

pub struct LmdbCache {
    env: heed::Env,
    db: Database<Bytes, Bytes>,
    pending: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl LmdbCache {
    pub fn open(path: &Path, max_size: Option<usize>) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("creating cache dir {}", path.display()))?;

        let max_size = max_size.unwrap_or(64 * 1024 * 1024 * 1024);

        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(max_size)
                .max_dbs(1)
                .open(path)
                .with_context(|| format!("opening LMDB env at {}", path.display()))?
        };

        let mut wtxn = env.write_txn()?;
        let db: Database<Bytes, Bytes> = env.create_database(&mut wtxn, Some("artifacts"))?;
        wtxn.commit()?;

        Ok(Self {
            env,
            db,
            pending: Mutex::new(Vec::new()),
        })
    }

    fn unit_manifest_key(unit_key: &[u8; 32]) -> Vec<u8> {
        let hex = super::hex(unit_key);
        let mut k = Vec::with_capacity(2 + 64);
        k.extend_from_slice(b"m:");
        k.extend_from_slice(hex.as_bytes());
        k
    }

    fn artifact_key(unit_key: &[u8; 32], rel_path: &str) -> Vec<u8> {
        let hex = super::hex(unit_key);
        let mut k = Vec::with_capacity(2 + 64 + 1 + rel_path.len());
        k.extend_from_slice(b"a:");
        k.extend_from_slice(hex.as_bytes());
        k.push(b':');
        k.extend_from_slice(rel_path.as_bytes());
        k
    }

    fn dyn_key(static_key: &[u8; 32], shape: &[u8; 32]) -> Vec<u8> {
        let static_hex = super::hex(static_key);
        let shape_hex = super::hex(shape);
        let mut k = Vec::with_capacity(2 + 64 + 1 + 64);
        k.extend_from_slice(b"d:");
        k.extend_from_slice(static_hex.as_bytes());
        k.push(b':');
        k.extend_from_slice(shape_hex.as_bytes());
        k
    }

    fn dyn_prefix(static_key: &[u8; 32]) -> Vec<u8> {
        let static_hex = super::hex(static_key);
        let mut k = Vec::with_capacity(2 + 64 + 1);
        k.extend_from_slice(b"d:");
        k.extend_from_slice(static_hex.as_bytes());
        k.push(b':');
        k
    }
}

impl CacheBackend for LmdbCache {
    fn contains_unit(&self, unit_key: &[u8; 32]) -> Result<bool> {
        let rtxn = self.env.read_txn()?;
        let key = Self::unit_manifest_key(unit_key);
        Ok(self.db.get(&rtxn, &key)?.is_some())
    }

    fn list_artifacts(&self, unit_key: &[u8; 32]) -> Result<Vec<String>> {
        let rtxn = self.env.read_txn()?;
        let key = Self::unit_manifest_key(unit_key);
        match self.db.get(&rtxn, &key)? {
            Some(data) => Ok(serde_json::from_slice(data)?),
            None => Ok(Vec::new()),
        }
    }

    fn get_artifact(&self, unit_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>> {
        let rtxn = self.env.read_txn()?;
        let key = Self::artifact_key(unit_key, rel_path);
        match self.db.get(&rtxn, &key)? {
            Some(data) => Ok(Some(data.to_vec())),
            None => Ok(None),
        }
    }

    fn restore_artifact(
        &self,
        unit_key: &[u8; 32],
        rel_path: &str,
        dest: &std::path::Path,
    ) -> Result<bool> {
        let rtxn = self.env.read_txn()?;
        let key = Self::artifact_key(unit_key, rel_path);
        match self.db.get(&rtxn, &key)? {
            Some(data) => {
                std::fs::write(dest, data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn put_artifact(&self, unit_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()> {
        let key = Self::artifact_key(unit_key, rel_path);
        self.pending.lock().unwrap().push((key, data.to_vec()));
        Ok(())
    }

    fn finalize_unit(&self, unit_key: &[u8; 32], artifacts: &[String]) -> Result<()> {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        let mut wtxn = self.env.write_txn()?;
        for (key, data) in &pending {
            self.db.put(&mut wtxn, key, data)?;
        }
        let manifest_key = Self::unit_manifest_key(unit_key);
        let manifest_data = serde_json::to_vec(artifacts)?;
        self.db.put(&mut wtxn, &manifest_key, &manifest_data)?;
        wtxn.commit()?;
        Ok(())
    }

    fn list_dynamic_inputs(&self, static_key: &[u8; 32]) -> Result<Vec<DynamicInputs>> {
        let rtxn = self.env.read_txn()?;
        let prefix = Self::dyn_prefix(static_key);
        let iter = self.db.prefix_iter(&rtxn, &prefix)?;
        let mut out = Vec::new();
        for entry in iter {
            let (_k, v) = entry?;
            let inputs: DynamicInputs = serde_json::from_slice(v)?;
            out.push(inputs);
        }
        Ok(out)
    }

    fn put_dynamic_inputs(&self, static_key: &[u8; 32], inputs: &DynamicInputs) -> Result<()> {
        let key = Self::dyn_key(static_key, &inputs.shape_hash());
        let mut wtxn = self.env.write_txn()?;
        let data = serde_json::to_vec(inputs)?;
        self.db.put(&mut wtxn, &key, &data)?;
        wtxn.commit()?;
        Ok(())
    }

    fn name(&self) -> &str {
        "lmdb"
    }
}

#[cfg(test)]
#[path = "lmdb_tests.rs"]
mod tests;
