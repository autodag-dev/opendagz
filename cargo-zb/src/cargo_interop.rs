use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use cargo::core::compiler::unit_graph::UnitGraph;
use cargo::core::compiler::{DefaultExecutor, Executor, Unit, UnitInterner};
use cargo::core::Workspace;
use cargo::ops::{self, CompileOptions};
use cargo::util::important_paths::find_root_manifest_for_wd;
use cargo::GlobalContext;

pub struct BuildPlan {
    pub unit_graph: UnitGraph,
    pub roots: Vec<Unit>,
    pub rustc_version: String,
    pub target_dir: PathBuf,
}

pub fn resolve_manifest(manifest_path: Option<&Path>, gctx: &GlobalContext) -> Result<PathBuf> {
    match manifest_path {
        Some(p) => Ok(p.to_path_buf()),
        None => Ok(find_root_manifest_for_wd(gctx.cwd())?),
    }
}

pub fn plan_build(
    ws: &Workspace<'_>,
    compile_opts: &CompileOptions,
) -> Result<BuildPlan> {
    let gctx = ws.gctx();

    let rustc = gctx.load_global_rustc(Some(ws))?;
    let rustc_version = rustc.verbose_version.clone();

    let interner = UnitInterner::new();
    let bcx = ops::create_bcx(ws, compile_opts, &interner, None)?;

    let target_dir = ws.target_dir().into_path_unlocked();

    let roots = bcx.roots.clone();
    let unit_graph = bcx.unit_graph.clone();

    Ok(BuildPlan {
        unit_graph,
        roots,
        rustc_version,
        target_dir,
    })
}

pub fn execute_build(
    ws: &Workspace<'_>,
    compile_opts: &CompileOptions,
) -> Result<()> {
    let exec: Arc<dyn Executor> = Arc::new(DefaultExecutor);
    ops::compile_with_exec(ws, compile_opts, &exec)?;
    Ok(())
}
