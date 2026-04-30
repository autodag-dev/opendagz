use std::collections::HashMap;

use anyhow::{Context, Result};
use cargo::core::compiler::{CompileKind, Unit};
use cargo::core::compiler::unit_graph::UnitGraph;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey(pub [u8; 32]);

impl CacheKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[allow(dead_code)]
    pub fn to_hex(&self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }
}

/// Combine a unit's static_key with the content hash of its dynamic inputs and
/// the **full_keys of its dependencies** to yield the unit's full content-addressed
/// cache key.
///
/// Why dep full_keys: when a build script injects env vars via `cargo:rustc-env=`,
/// the dependent crate's compilation receives those values and rustc records them
/// as `# env-dep:NAME=VALUE` in the dep-info. Those env vars are NOT in the user's
/// process env at cargo-zb invocation time, so hashing current values misses the
/// change. Folding dep full_keys propagates "any dep's content has changed" up to
/// consumers, which is exactly the invalidation cargo's own DepFingerprint
/// achieves at the unit-graph level.
pub fn combine_full_key(
    static_key: &CacheKey,
    dynamic_content_hash: &[u8; 32],
    dep_full_keys: &[CacheKey],
) -> CacheKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"cargo-zb-full-v2\0");
    hasher.update(static_key.as_bytes());
    hasher.update(dynamic_content_hash);
    let mut deps: Vec<&CacheKey> = dep_full_keys.iter().collect();
    deps.sort_by(|a, b| a.0.cmp(&b.0));
    for k in deps {
        hasher.update(k.as_bytes());
    }
    hasher.update(b"deps-end\0");
    CacheKey(*hasher.finalize().as_bytes())
}

/// Compute static cache keys for all units in the graph (bottom-up topo order).
///
/// The "static" qualifier distinguishes this from the unit's full content-addressed
/// key. A static_key folds in everything cargo-zb can know without running anything:
/// rustc version, pkg metadata, target, profile, features, rustflags, recursive dep
/// keys, and `*.rs` source contents (for path packages). It does NOT cover external
/// file deps reached via macros or build script `rerun-if-*` declarations — those
/// land in the dynamic inputs, harvested post-build.
pub fn compute_cache_keys(
    unit_graph: &UnitGraph,
    roots: &[Unit],
    rustc_version: &str,
) -> Result<HashMap<Unit, CacheKey>> {
    let mut keys: HashMap<Unit, CacheKey> = HashMap::new();

    let mut visited = std::collections::HashSet::new();
    let mut order = Vec::new();

    fn visit(
        unit: &Unit,
        graph: &UnitGraph,
        visited: &mut std::collections::HashSet<Unit>,
        order: &mut Vec<Unit>,
    ) {
        if !visited.insert(unit.clone()) {
            return;
        }
        if let Some(deps) = graph.get(unit) {
            for dep in deps {
                visit(&dep.unit, graph, visited, order);
            }
        }
        order.push(unit.clone());
    }

    for root in roots {
        visit(root, unit_graph, &mut visited, &mut order);
    }

    for unit in &order {
        let key = compute_unit_key(unit, unit_graph, &keys, rustc_version)?;
        keys.insert(unit.clone(), key);
    }

    Ok(keys)
}

fn compute_unit_key(
    unit: &Unit,
    unit_graph: &UnitGraph,
    dep_keys: &HashMap<Unit, CacheKey>,
    rustc_version: &str,
) -> Result<CacheKey> {
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"cargo-zb-v1\0");

    hasher.update(rustc_version.as_bytes());
    hasher.update(b"\0");

    let pkg_id = unit.pkg.package_id();
    hasher.update(pkg_id.name().as_bytes());
    hasher.update(b"\0");
    hasher.update(pkg_id.version().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(pkg_id.source_id().to_string().as_bytes());
    hasher.update(b"\0");

    hasher.update(unit.target.name().as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", unit.target.kind()).as_bytes());
    hasher.update(b"\0");

    hasher.update(format!("{:?}", unit.mode).as_bytes());
    hasher.update(b"\0");

    hash_profile(&mut hasher, &unit.profile);

    match unit.kind {
        CompileKind::Host => {
            hasher.update(b"host\0");
        }
        CompileKind::Target(t) => {
            hasher.update(b"target:");
            hasher.update(t.rustc_target().as_str().as_bytes());
            hasher.update(b"\0");
        }
    }

    let mut features: Vec<_> = unit.features.iter().map(|f| f.as_str()).collect();
    features.sort();
    for f in &features {
        hasher.update(f.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"features-end\0");

    for flag in unit.rustflags.iter() {
        hasher.update(flag.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"rustflags-end\0");

    if let Some(deps) = unit_graph.get(unit) {
        let mut dep_entries: Vec<_> = deps
            .iter()
            .filter_map(|dep| {
                dep_keys.get(&dep.unit).map(|key| {
                    (dep.unit.pkg.package_id().name().as_str().to_string(), *key)
                })
            })
            .collect();
        dep_entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.0.cmp(&b.1.0)));
        for (name, key) in &dep_entries {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(key.as_bytes());
            hasher.update(b"\0");
        }
    }
    hasher.update(b"deps-end\0");

    // Path packages: hash source files. Registry/git: version is in pkg_id.
    if pkg_id.source_id().is_path() {
        hash_source_files(&mut hasher, unit)?;
    }

    Ok(CacheKey(*hasher.finalize().as_bytes()))
}

fn hash_profile(hasher: &mut blake3::Hasher, profile: &cargo::core::profiles::Profile) {
    hasher.update(format!("{}", profile.opt_level).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.debuginfo).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.debug_assertions).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.overflow_checks).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.lto).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.panic).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.codegen_units).as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:?}", profile.strip).as_bytes());
    hasher.update(b"\0");
}

fn hash_source_files(hasher: &mut blake3::Hasher, unit: &Unit) -> Result<()> {
    let pkg_root = unit.pkg.root();

    let mut paths: Vec<_> = walkdir::WalkDir::new(pkg_root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == "rs")
                && !e.path().components().any(|c| c.as_os_str() == "target")
        })
        .map(|e| e.into_path())
        .collect();
    paths.sort();

    for path in &paths {
        let rel = path.strip_prefix(pkg_root).unwrap_or(path);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        let contents = std::fs::read(path)
            .with_context(|| format!("reading {}", path.display()))?;
        hasher.update(&contents);
        hasher.update(b"\0");
    }
    hasher.update(b"source-end\0");
    Ok(())
}
