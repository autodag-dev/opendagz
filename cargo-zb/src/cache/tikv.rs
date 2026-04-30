use std::path::Path;

use anyhow::{Context, Result};
use tikv_client::RawClient;
use tokio::runtime::Runtime;

use super::{CacheBackend, DynamicInputs};

pub struct TikvCache {
    client: RawClient,
    rt: Runtime,
}

impl TikvCache {
    pub fn new(pd_endpoints: Vec<String>) -> Result<Self> {
        let rt = Runtime::new().context("creating tokio runtime for tikv")?;
        let client = rt
            .block_on(RawClient::new(pd_endpoints))
            .context("connecting to tikv")?;
        Ok(Self { client, rt })
    }

    fn key(prefix: &str, hash: &[u8; 32], suffix: &str) -> Vec<u8> {
        let hex = super::hex(hash);
        let mut k = Vec::with_capacity(9 + prefix.len() + 64 + suffix.len());
        k.extend_from_slice(b"cargo-zb/");
        k.extend_from_slice(prefix.as_bytes());
        k.extend_from_slice(hex.as_bytes());
        k.extend_from_slice(suffix.as_bytes());
        k
    }

    fn dyn_key(static_key: &[u8; 32], shape: &[u8; 32]) -> Vec<u8> {
        let static_hex = super::hex(static_key);
        let shape_hex = super::hex(shape);
        let mut k = Vec::with_capacity(11 + 64 + 1 + 64);
        k.extend_from_slice(b"cargo-zb/d:");
        k.extend_from_slice(static_hex.as_bytes());
        k.push(b':');
        k.extend_from_slice(shape_hex.as_bytes());
        k
    }

    fn dyn_prefix(static_key: &[u8; 32]) -> Vec<u8> {
        let static_hex = super::hex(static_key);
        let mut k = Vec::with_capacity(11 + 64 + 1);
        k.extend_from_slice(b"cargo-zb/d:");
        k.extend_from_slice(static_hex.as_bytes());
        k.push(b':');
        k
    }

    fn get_raw(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.rt.block_on(self.client.get(key)).context("tikv get")
    }

    fn put_raw(&self, key: Vec<u8>, data: Vec<u8>) -> Result<()> {
        self.rt.block_on(self.client.put(key, data)).context("tikv put")
    }
}

impl CacheBackend for TikvCache {
    fn contains_unit(&self, unit_key: &[u8; 32]) -> Result<bool> {
        Ok(self.get_raw(Self::key("m:", unit_key, ""))?.is_some())
    }

    fn list_artifacts(&self, unit_key: &[u8; 32]) -> Result<Vec<String>> {
        match self.get_raw(Self::key("m:", unit_key, ""))? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    fn get_artifact(&self, unit_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>> {
        self.get_raw(Self::key("a:", unit_key, &format!(":{rel_path}")))
    }

    fn put_artifact(&self, unit_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()> {
        self.put_raw(
            Self::key("a:", unit_key, &format!(":{rel_path}")),
            data.to_vec(),
        )
    }

    fn store_artifact_from_file(
        &self,
        unit_key: &[u8; 32],
        rel_path: &str,
        src: &Path,
    ) -> Result<()> {
        let data = std::fs::read(src)?;
        self.put_artifact(unit_key, rel_path, &data)
    }

    fn finalize_unit(&self, unit_key: &[u8; 32], artifacts: &[String]) -> Result<()> {
        let manifest = serde_json::to_vec(artifacts)?;
        self.put_raw(Self::key("m:", unit_key, ""), manifest)
    }

    fn list_dynamic_inputs(&self, static_key: &[u8; 32]) -> Result<Vec<DynamicInputs>> {
        let prefix = Self::dyn_prefix(static_key);
        // tikv-client RawClient supports scan; do a bounded scan from `prefix`.
        let end = {
            let mut e = prefix.clone();
            e.push(0xff);
            e
        };
        let scan = self.rt.block_on(self.client.scan(prefix..end, 1024))
            .context("tikv scan dynamic inputs")?;
        let mut out = Vec::new();
        for kv in scan {
            let v: DynamicInputs = serde_json::from_slice(kv.1.as_ref())?;
            out.push(v);
        }
        Ok(out)
    }

    fn put_dynamic_inputs(&self, static_key: &[u8; 32], inputs: &DynamicInputs) -> Result<()> {
        let key = Self::dyn_key(static_key, &inputs.shape_hash());
        if self.get_raw(key.clone())?.is_some() {
            return Ok(());
        }
        let data = serde_json::to_vec(inputs)?;
        self.put_raw(key, data)
    }

    fn name(&self) -> &str {
        "tikv"
    }
}
