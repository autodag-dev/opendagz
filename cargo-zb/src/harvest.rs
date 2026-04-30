//! Post-build harvest: walk every unit, derive its `DynamicInputs` from the
//! filesystem state cargo's build leaves behind in `target/`.
//!
//! Two information sources, both per-unit:
//!
//! 1. **rustc's `.d` dep-info file** at `target/<target>/release/deps/*-<unit_hash>.d` —
//!    Makefile-style text emitted by rustc's `--emit=dep-info`. Contains every source
//!    file rustc actually opened (incl. files reached via `include_str!`/`include_bytes!`/
//!    macros like `rust-embed`'s `#[derive(RustEmbed)]` that expand to `include_bytes!`)
//!    plus `# env-dep:NAME=VALUE` lines for every `env!()`/`option_env!()` reference.
//!    We parse this directly; cargo's `parse_rustc_dep_info` is `pub(crate)` from
//!    outside the cargo crate, but the format is simple and stable.
//!
//! 2. **Build script `output` file** at `target/<target>/release/build/<pkg>-<unit_hash>/output`
//!    for `RunCustomBuild` units. Captures `cargo:rerun-if-changed=PATH` and
//!    `cargo:rerun-if-env-changed=NAME` declarations verbatim.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo::core::compiler::{BuildRunner, CompileMode, Unit};

use crate::cache::{hash_path_current, DynEnv, DynPath, DynamicInputs};

/// Harvest the unit's dynamic inputs (paths + env vars) from the post-build state.
///
/// Returns `Ok(None)` if the unit hasn't actually been built (no fingerprint state on disk).
pub fn harvest_unit(
    runner: &BuildRunner<'_, '_>,
    unit: &Unit,
) -> Result<Option<DynamicInputs>> {
    if unit.mode == CompileMode::RunCustomBuild {
        harvest_run_custom_build(runner, unit)
    } else {
        harvest_compile(runner, unit)
    }
}

fn harvest_compile(
    runner: &BuildRunner<'_, '_>,
    unit: &Unit,
) -> Result<Option<DynamicInputs>> {
    let files = runner.files();
    let metadata = files.metadata(unit);
    let Some(unit_hash) = metadata.c_extra_filename() else {
        return Ok(None);
    };
    let unit_hash = unit_hash.to_string();

    // Where rustc emitted this unit's `.d`: deps_dir for normal compiles,
    // build/<pkg>-<hash>/ for build-script COMPILE units (cargo passes
    // --out-dir there for those).
    let mut search_dirs = vec![files.deps_dir(unit)];
    if unit.target.is_custom_build() && !unit.mode.is_run_custom_build() {
        search_dirs.push(files.build_script_dir(unit));
    }
    let dep_info_path = search_dirs
        .iter()
        .find_map(|d| find_rustc_dep_info(d, &unit_hash).transpose())
        .transpose()?;
    let dep_info_path = match dep_info_path {
        Some(p) => p,
        None => {
            tracing::debug!(
                "no rustc dep-info for {} ({}) hash={}, searched {:?}",
                unit.pkg.name(),
                unit.target.name(),
                unit_hash,
                search_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
            );
            return Ok(None);
        }
    };

    let pkg_root = unit.pkg.root();
    let info = parse_rustc_dep_info(&dep_info_path)
        .with_context(|| format!("parsing {}", dep_info_path.display()))?;

    // Drop files inside pkg_root with .rs extension — already in static_key. Keep:
    //   - any path outside pkg_root (macro-resolved external includes, OUT_DIR refs)
    //   - non-.rs files inside pkg_root (e.g. .fbs, .html, .json)
    let mut path_set: Vec<PathBuf> = info
        .files
        .into_iter()
        .filter(|p: &PathBuf| {
            if !p.starts_with(pkg_root) {
                return true;
            }
            p.extension().and_then(|e: &std::ffi::OsStr| e.to_str()) != Some("rs")
        })
        .collect();
    path_set.sort();
    path_set.dedup();

    let paths: Vec<DynPath> = path_set
        .into_iter()
        .map(|p| {
            let h = hash_path_current(&p).unwrap_or([0u8; 32]);
            DynPath { path: p, stored_hash: h }
        })
        .collect();
    let envs: Vec<DynEnv> = info
        .env
        .into_iter()
        .map(|(name, stored_value)| DynEnv { name, stored_value })
        .collect();

    Ok(Some(DynamicInputs { paths, envs }))
}

fn harvest_run_custom_build(
    runner: &BuildRunner<'_, '_>,
    unit: &Unit,
) -> Result<Option<DynamicInputs>> {
    let output_path = runner.files().build_script_run_dir(unit).join("output");
    if !output_path.exists() {
        tracing::debug!(
            "no build-script output at {} for {} ({})",
            output_path.display(),
            unit.pkg.name(),
            unit.target.name()
        );
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&output_path)?;
    let pkg_root = unit.pkg.root();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut env_names: Vec<String> = Vec::new();

    for line in contents.lines() {
        let body = line
            .strip_prefix("cargo::")
            .or_else(|| line.strip_prefix("cargo:"));
        let Some(body) = body else { continue };
        if let Some(p) = body.strip_prefix("rerun-if-changed=") {
            let candidate = Path::new(p);
            let abs = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                pkg_root.join(candidate)
            };
            paths.push(abs);
        } else if let Some(name) = body.strip_prefix("rerun-if-env-changed=") {
            env_names.push(name.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    env_names.sort();
    env_names.dedup();

    let path_entries: Vec<DynPath> = paths
        .into_iter()
        .map(|p| {
            let h = hash_path_current(&p).unwrap_or([0u8; 32]);
            DynPath { path: p, stored_hash: h }
        })
        .collect();
    let env_entries: Vec<DynEnv> = env_names
        .into_iter()
        .map(|n| {
            let v = std::env::var(&n).ok();
            DynEnv { name: n, stored_value: v }
        })
        .collect();

    Ok(Some(DynamicInputs {
        paths: path_entries,
        envs: env_entries,
    }))
}

/// Find rustc's text-format `.d` for this unit in `deps_dir`. Cargo names them
/// `<crate-name>-<unit_hash>.d` (with various prefixes: `lib` for lib targets,
/// `build_script_build` for build script COMPILE units, plain `<bin>` for bins).
/// We just scan for any `.d` whose stem ends in `-<unit_hash>`.
fn find_rustc_dep_info(deps_dir: &Path, unit_hash: &str) -> Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(deps_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", deps_dir.display())),
    };
    let needle = format!("-{unit_hash}.d");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if name_str.ends_with(&needle) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

#[derive(Default)]
struct RustcDepInfo {
    files: Vec<PathBuf>,
    env: Vec<(String, Option<String>)>,
}

/// Parse rustc's `.d` Makefile-style dep-info file.
///
/// Layout (rustc emits):
///   <output>: <input1> <input2> ...
///   <output2>: <inputs>...
///   [blank line]
///   # env-dep:NAME=VALUE
///   # env-dep:NAME           (var was unset; compilation depends on it being unset)
///   # checksum:<algo>:<hex> file_len:<n> <path>
///
/// Paths with spaces are escaped with `\ `. We follow cargo's own parse routine
/// (see `cargo/src/cargo/core/compiler/fingerprint/dep_info.rs::parse_rustc_dep_info`)
/// closely so the behavior matches.
fn parse_rustc_dep_info(path: &Path) -> Result<RustcDepInfo> {
    let contents = std::fs::read_to_string(path)?;
    let mut ret = RustcDepInfo::default();
    let mut found_deps = false;

    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("# env-dep:") {
            let mut parts = rest.splitn(2, '=');
            let Some(env_var) = parts.next() else {
                continue;
            };
            let env_val = match parts.next() {
                Some(s) => Some(unescape_env(s)?),
                None => None,
            };
            ret.env.push((unescape_env(env_var)?, env_val));
        } else if let Some(pos) = line.find(": ") {
            // Multiple "outputs: deps" lines are emitted by rustc when there are
            // multiple output files for the same compilation; the dep list is
            // identical, so only parse once.
            if found_deps {
                continue;
            }
            found_deps = true;
            let mut deps = line[pos + 2..].split_whitespace();
            while let Some(s) = deps.next() {
                let mut file = s.to_string();
                while file.ends_with('\\') {
                    // Backslash-space = literal space in path. Pop the trailing
                    // backslash, append a space, and consume the next whitespace
                    // token to glue continuations.
                    file.pop();
                    file.push(' ');
                    let Some(next) = deps.next() else {
                        anyhow::bail!("malformed dep-info format, trailing \\");
                    };
                    file.push_str(next);
                }
                ret.files.push(PathBuf::from(file));
            }
        }
        // Ignore `# checksum:` lines — we use byte-content hashing, not rustc's checksums.
    }
    Ok(ret)
}

/// rustc escapes `\`, `\n`, `\r` in env-dep values so a single line can hold a
/// multi-line value.
fn unescape_env(s: &str) -> Result<String> {
    let mut ret = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            ret.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => ret.push('\\'),
            Some('n') => ret.push('\n'),
            Some('r') => ret.push('\r'),
            Some(c) => anyhow::bail!("unknown escape character `\\{c}` in env-dep"),
            None => anyhow::bail!("unterminated escape in env-dep"),
        }
    }
    Ok(ret)
}

#[allow(dead_code)]
pub fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out.sort();
    out
}
