use std::path::Path;

use anyhow::{Context, Result};
use tikv_client::RawClient;
use tokio::runtime::Runtime;

use super::CacheBackend;

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

    fn key(prefix: &str, build_key: &[u8; 32], suffix: &str) -> Vec<u8> {
        let hex = super::hex(build_key);
        let mut k = Vec::with_capacity(9 + prefix.len() + 64 + suffix.len());
        k.extend_from_slice(b"cargo-zb/");
        k.extend_from_slice(prefix.as_bytes());
        k.extend_from_slice(hex.as_bytes());
        k.extend_from_slice(suffix.as_bytes());
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
    fn contains_build(&self, build_key: &[u8; 32]) -> Result<bool> {
        Ok(self.get_raw(Self::key("m:", build_key, ""))?.is_some())
    }

    fn list_artifacts(&self, build_key: &[u8; 32]) -> Result<Vec<String>> {
        match self.get_raw(Self::key("m:", build_key, ""))? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    fn get_artifact(&self, build_key: &[u8; 32], rel_path: &str) -> Result<Option<Vec<u8>>> {
        self.get_raw(Self::key("a:", build_key, &format!(":{rel_path}")))
    }

    fn put_artifact(&self, build_key: &[u8; 32], rel_path: &str, data: &[u8]) -> Result<()> {
        self.put_raw(
            Self::key("a:", build_key, &format!(":{rel_path}")),
            data.to_vec(),
        )
    }

    fn store_artifact_from_file(
        &self,
        build_key: &[u8; 32],
        rel_path: &str,
        src: &Path,
    ) -> Result<()> {
        let data = std::fs::read(src)?;
        self.put_artifact(build_key, rel_path, &data)
    }

    fn finalize_build(&self, build_key: &[u8; 32], artifacts: &[String]) -> Result<()> {
        let manifest = serde_json::to_vec(artifacts)?;
        self.put_raw(Self::key("m:", build_key, ""), manifest)
    }

    fn name(&self) -> &str {
        "tikv"
    }
}
