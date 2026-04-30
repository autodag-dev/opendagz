use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use cargo::core::Workspace;
use cargo::core::compiler::unit_graph::UnitGraph;
use cargo::core::compiler::{
    BuildContext, BuildRunner, DefaultExecutor, Executor, Unit, UnitInterner,
};
use cargo::ops::{self, CompileOptions};
use cargo::util::important_paths::find_root_manifest_for_wd;
use cargo::GlobalContext;

pub fn resolve_manifest(manifest_path: Option<&Path>, gctx: &GlobalContext) -> Result<PathBuf> {
    match manifest_path {
        Some(p) => Ok(p.to_path_buf()),
        None => Ok(find_root_manifest_for_wd(gctx.cwd())?),
    }
}

/// All the long-lived state for a single cargo-zb invocation. Borrows nest:
/// `gctx` outlives `ws` outlives `interner` outlives the `BuildContext`. We
/// expose helpers that build a fresh `BuildRunner` per phase — `BuildRunner`
/// borrows from the `BuildContext`, so we can't keep one alive across an
/// `ops::compile_with_exec` call (cargo's compile internally constructs its
/// own `BuildContext`/`BuildRunner`). Recreating between phases is cheap and
/// deterministic — same workspace + opts → same unit graph + paths.
pub fn build_bcx<'a, 'gctx>(
    ws: &'a Workspace<'gctx>,
    interner: &'a UnitInterner,
    compile_opts: &'a CompileOptions,
) -> Result<BuildContext<'a, 'gctx>> {
    Ok(ops::create_bcx(ws, compile_opts, interner, None)?)
}

/// Build a `BuildRunner` and prepare it through the planning phase (no
/// compilation). After this returns, `runner.files()` resolves per-unit paths.
///
/// We populate `runner.lto` ourselves via vendored cargo code; cargo's actual
/// `lto::generate` is private to the cargo crate, but `prepare_units` ->
/// `compute_metadata` reads `runner.lto[unit]` and panics if it's empty.
pub fn prepared_runner<'a, 'gctx>(
    bcx: &'a BuildContext<'a, 'gctx>,
) -> Result<BuildRunner<'a, 'gctx>> {
    let mut runner = BuildRunner::new(bcx)?;
    runner.lto = crate::lto_vendored::generate(bcx)?;
    runner.prepare_units()?;
    runner.prepare()?;
    Ok(runner)
}

pub fn rustc_verbose_version(ws: &Workspace<'_>) -> Result<String> {
    let gctx = ws.gctx();
    let rustc = gctx.load_global_rustc(Some(ws))?;
    Ok(rustc.verbose_version.clone())
}

pub fn target_dir(ws: &Workspace<'_>) -> PathBuf {
    ws.target_dir().into_path_unlocked()
}

pub fn execute_build(ws: &Workspace<'_>, compile_opts: &CompileOptions) -> Result<()> {
    let exec: Arc<dyn Executor> = Arc::new(DefaultExecutor);
    ops::compile_with_exec(ws, compile_opts, &exec)?;
    Ok(())
}

/// Topologically order units (deps before consumers).
pub fn topo_order(unit_graph: &UnitGraph, roots: &[Unit]) -> Vec<Unit> {
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
    order
}

/// Helper: look up a previously-stored env value via cargo's env_config or stdlib env.
#[allow(dead_code)]
pub fn env_lookup(gctx: &GlobalContext, name: &str) -> Option<String> {
    if let Ok(cfg) = gctx.env_config() {
        if let Some(v) = cfg.get(name) {
            return v.to_str().map(ToOwned::to_owned);
        }
    }
    gctx.get_env(name).ok().map(|s| s.to_string())
}
