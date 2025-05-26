mod artifacts;
mod bench;
mod cache;
mod cargo_interop;
mod hash;

use std::path::PathBuf;

use anyhow::Result;
use cache::CacheBackend;
use clap::{Parser, Subcommand};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "cargo-zb", about = "Cargo build with better caching")]
struct Cli {
    #[command(subcommand)]
    command: CargoSub,
}

#[derive(Subcommand, Debug)]
enum CargoSub {
    #[command(name = "zb")]
    Zb(ZbArgs),
}

#[derive(clap::Args, Debug)]
struct ZbArgs {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Build in release mode
    #[arg(long, global = true)]
    release: bool,

    /// Build for the target triple
    #[arg(long, global = true)]
    target: Option<String>,

    /// Space or comma separated list of features to activate
    #[arg(long, global = true)]
    features: Vec<String>,

    /// Activate all available features
    #[arg(long, global = true)]
    all_features: bool,

    /// Do not activate the `default` feature
    #[arg(long, global = true)]
    no_default_features: bool,

    /// Package to build (can be specified multiple times)
    #[arg(short, long, global = true)]
    package: Vec<String>,

    /// Number of parallel jobs
    #[arg(short, long, global = true)]
    jobs: Option<u32>,

    /// Path to Cargo.toml
    #[arg(long, global = true)]
    manifest_path: Option<PathBuf>,

    /// Cache directory (default: ~/.cache/cargo-zb/)
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Parallel threads for cache restore
    #[arg(long, default_value = "4")]
    io_threads: usize,

    /// Cache backend: "fs" or "lmdb"
    #[arg(long, default_value = "fs")]
    cache_backend: String,

    /// Disable caching (just run cargo build)
    #[arg(long)]
    no_cache: bool,

    /// Verbose output
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(hide = true)]
    Build,

    Bench {
        /// Skip sccache comparison
        #[arg(long)]
        no_sccache: bool,
    },

    ListBenched,
}

fn main() -> Result<()> {
    let Cli { command: CargoSub::Zb(cli) } = Cli::parse();

    match &cli.command {
        Some(Commands::Bench { no_sccache }) => {
            let manifest = cli.manifest_path.clone()
                .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
            return bench::run_bench(
                &manifest,
                cli.release,
                &cli.cache_backend,
                cli.io_threads,
                !no_sccache,
            );
        }
        Some(Commands::ListBenched) => {
            return bench::list_benched();
        }
        _ => {}
    }

    // When cargo runs as a library, RUSTUP_HOME and RUSTUP_TOOLCHAIN
    // may be missing — build scripts need them.
    unsafe {
        if std::env::var_os("RUSTUP_TOOLCHAIN").is_none() {
            let output = std::process::Command::new("rustup")
                .args(["default"])
                .output();
            let toolchain = output.ok().and_then(|o| {
                let s = String::from_utf8(o.stdout).ok()?;
                Some(s.split_whitespace().next()?.to_string())
            });
            std::env::set_var(
                "RUSTUP_TOOLCHAIN",
                toolchain.as_deref().unwrap_or("stable"),
            );
        }
        if std::env::var_os("RUSTUP_HOME").is_none() {
            if let Some(home) = std::env::var_os("HOME") {
                let rustup_home = std::path::PathBuf::from(home).join(".rustup");
                if rustup_home.exists() {
                    std::env::set_var("RUSTUP_HOME", &rustup_home);
                }
            }
        }
    }

    // cd to manifest dir so cargo's GlobalContext picks up .cargo/config.toml
    if let Some(manifest) = &cli.manifest_path {
        if let Some(dir) = manifest.canonicalize().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            std::env::set_current_dir(&dir)?;
        }
    }

    // Set up tracing
    let filter = match cli.verbose {
        0 => "cargo_zb=info",
        1 => "cargo_zb=debug",
        _ => "cargo_zb=trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| filter.into()),
        )
        .with_target(false)
        .init();

    if cli.no_cache {
        info!("caching disabled, running plain cargo build");
        return run_plain_build(&cli);
    }

    run_cached_build(&cli)
}

fn open_cache(cli: &ZbArgs) -> Result<Box<dyn CacheBackend>> {
    let dir = match &cli.cache_dir {
        Some(d) => d.clone(),
        None => cache::default_cache_dir()?,
    };
    let cache: Box<dyn CacheBackend> = match cli.cache_backend.as_str() {
        "lmdb" => Box::new(cache::lmdb::LmdbCache::open(&dir, None)?),
        "fs" => Box::new(cache::fs::FsCache::new(&dir)?),
        other => anyhow::bail!("unknown cache backend: {other} (expected \"fs\" or \"lmdb\")"),
    };
    info!("cache: {} at {}", cache.name(), dir.display());
    Ok(cache)
}

fn run_cached_build(cli: &ZbArgs) -> Result<()> {
    let cache = open_cache(cli)?;

    let gctx = cargo::GlobalContext::default()?;
    let root = cargo_interop::resolve_manifest(cli.manifest_path.as_deref(), &gctx)?;
    let ws = cargo::core::Workspace::new(&root, &gctx)?;

    let compile_opts = build_compile_options(cli, &gctx)?;

    info!("resolving dependencies and planning build...");
    let plan = cargo_interop::plan_build(&ws, &compile_opts)?;
    info!("unit graph: {} units, {} roots", plan.unit_graph.len(), plan.roots.len());

    info!("computing cache keys...");
    let cache_keys = hash::compute_cache_keys(
        &plan.unit_graph,
        &plan.roots,
        &plan.rustc_version,
    )?;
    let build_key = compute_build_key(&cache_keys);
    info!("build key: {}", build_key.to_hex());

    if cache.contains_build(build_key.as_bytes())? {
        info!("cache hit! restoring artifacts...");
        let count = artifacts::restore(cache.as_ref(), &build_key, &plan.target_dir, cli.io_threads)?;
        info!("restored {} artifact files from cache", count);
        info!("done (from cache)");
        return Ok(());
    }

    info!("cache miss, building...");
    let pre_build = artifacts::snapshot_target_dir(&plan.target_dir);
    cargo_interop::execute_build(&ws, &compile_opts)?;

    let new_files = artifacts::diff_target_dir(&plan.target_dir, &pre_build);
    if !new_files.is_empty() {
        info!("caching {} artifact files...", new_files.len());
        match artifacts::store(cache.as_ref(), &build_key, &new_files, &plan.target_dir) {
            Ok(count) => info!("stored {} files in cache", count),
            Err(e) => warn!("failed to cache artifacts: {}", e),
        }
    }

    info!("done");
    Ok(())
}

fn compute_build_key(cache_keys: &std::collections::HashMap<cargo::core::compiler::Unit, hash::CacheKey>) -> hash::CacheKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"cargo-zb-build-v1\0");
    let mut keys: Vec<[u8; 32]> = cache_keys.values().map(|k| k.0).collect();
    keys.sort();
    for k in &keys {
        hasher.update(k);
    }
    hash::CacheKey(*hasher.finalize().as_bytes())
}

fn run_plain_build(cli: &ZbArgs) -> Result<()> {
    let gctx = cargo::GlobalContext::default()?;
    let root = cargo_interop::resolve_manifest(cli.manifest_path.as_deref(), &gctx)?;
    let ws = cargo::core::Workspace::new(&root, &gctx)?;
    let compile_opts = build_compile_options(cli, &gctx)?;
    cargo_interop::execute_build(&ws, &compile_opts)
}

fn build_compile_options(
    cli: &ZbArgs,
    gctx: &cargo::GlobalContext,
) -> Result<cargo::ops::CompileOptions> {
    use cargo::core::compiler::{CompileKind, CompileTarget, UserIntent};

    let mut opts = cargo::ops::CompileOptions::new(gctx, UserIntent::Build)?;

    if let Some(j) = cli.jobs {
        opts.build_config.jobs = j;
    }

    if let Some(ref target) = cli.target {
        let compile_target = CompileTarget::new(target)?;
        opts.build_config.requested_kinds = vec![CompileKind::Target(compile_target)];
    }

    if cli.release {
        opts.build_config.requested_profile =
            cargo::util::interning::InternedString::new("release");
    }

    opts.cli_features = cargo::core::resolver::features::CliFeatures::from_command_line(
        &cli.features,
        cli.all_features,
        !cli.no_default_features,
    )?;

    if !cli.package.is_empty() {
        opts.spec = cargo::ops::Packages::Packages(
            cli.package.clone(),
        );
    } else {
        opts.spec = cargo::ops::Packages::Default;
    }

    Ok(opts)
}
