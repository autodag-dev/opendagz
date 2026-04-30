mod artifacts;
mod bench;
mod cache;
mod cargo_interop;
mod harvest;
mod hash;
mod lto_vendored;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use cache::CacheBackend;
use cargo::core::compiler::{CompileMode, Unit, UnitInterner};
use clap::{Parser, Subcommand};
#[allow(unused_imports)]
use tracing::{debug, info};

#[derive(Debug, Clone)]
enum MissCause {
    /// No prior manifest exists for this unit's static_key — first time we've
    /// seen this exact configuration. Could be any of: source change for a
    /// path package, cargo settings change, registry version bump.
    NewStaticKey { kind: PkgKind },
    /// Manifest(s) exist but none of their content_hashes match. We tracked
    /// the smallest diff against current state.
    DynamicChanged { source: InputSource, diff: cache::DiffReport },
    /// At least one dep missed; this unit is forced-miss because its full_key
    /// depends on dep full_keys.
    Cascade { dep_name: String },
}

#[derive(Debug, Clone, Copy)]
enum PkgKind { Path, Registry }

#[derive(Debug, Clone, Copy)]
enum InputSource { Rustc, BuildScript }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Category { Rust, Cargo, BuildScript, Cascade }

impl Category {
    fn label(self) -> &'static str {
        match self {
            Category::Rust => "rust",
            Category::Cargo => "cargo",
            Category::BuildScript => "buildscript",
            Category::Cascade => "cascade",
        }
    }
}

fn classify_miss(unit: &Unit, no_manifests: bool, diff: Option<cache::DiffReport>) -> MissCause {
    if no_manifests {
        let kind = if unit.pkg.package_id().source_id().is_path() {
            PkgKind::Path
        } else {
            PkgKind::Registry
        };
        MissCause::NewStaticKey { kind }
    } else {
        let source = if unit.mode == CompileMode::RunCustomBuild {
            InputSource::BuildScript
        } else {
            InputSource::Rustc
        };
        MissCause::DynamicChanged { source, diff: diff.unwrap_or_default() }
    }
}

fn miss_category(cause: &MissCause) -> Category {
    match cause {
        MissCause::Cascade { .. } => Category::Cascade,
        MissCause::NewStaticKey { kind: PkgKind::Path } => Category::Rust,
        MissCause::NewStaticKey { kind: PkgKind::Registry } => Category::Cargo,
        MissCause::DynamicChanged { source: InputSource::Rustc, .. } => Category::Rust,
        MissCause::DynamicChanged { source: InputSource::BuildScript, .. } => Category::BuildScript,
    }
}

/// One-line "first trigger" explanation for a unit's miss.
fn first_trigger(cause: &MissCause) -> String {
    match cause {
        MissCause::NewStaticKey { kind: PkgKind::Path } => "no prior manifest (path pkg — likely source change)".into(),
        MissCause::NewStaticKey { kind: PkgKind::Registry } => "no prior manifest (registry pkg — likely cargo settings change)".into(),
        MissCause::Cascade { dep_name } => format!("dep {dep_name} missed"),
        MissCause::DynamicChanged { diff, .. } => {
            if let Some(p) = diff.changed_paths.first() {
                format!("path content changed: {}", p.display())
            } else if let Some(p) = diff.appeared_paths.first() {
                format!("path appeared: {}", p.display())
            } else if let Some(p) = diff.missing_paths.first() {
                format!("path disappeared: {}", p.display())
            } else if let Some(e) = diff.changed_envs.first() {
                format!("env changed: {e}")
            } else {
                "content_hash mismatch (no per-entry diff)".into()
            }
        }
    }
}

/// Single head printed before cargo's own output: a one-line hit/miss tally
/// and at most one line per non-empty miss category with up to 3 examples.
/// With `CARGO_LOG` set, also dump per-unit miss detail and the union of
/// invalidated inputs.
fn print_lookup_summary(
    hits: &HashMap<Unit, hash::CacheKey>,
    misses: &[(Unit, MissCause)],
) {
    info!("cargo-zb: {} hits, {} misses", hits.len(), misses.len());
    if misses.is_empty() {
        return;
    }
    let mut by_cat: HashMap<Category, Vec<&(Unit, MissCause)>> = HashMap::new();
    for entry in misses {
        by_cat.entry(miss_category(&entry.1)).or_default().push(entry);
    }
    for cat in [Category::Rust, Category::Cargo, Category::BuildScript, Category::Cascade] {
        let entries = by_cat.get(&cat);
        let count = entries.map(|v| v.len()).unwrap_or(0);
        if count == 0 {
            continue;
        }
        let examples: Vec<String> = entries
            .unwrap()
            .iter()
            .take(3)
            .map(|(u, _)| format!("{}({})", u.pkg.name(), u.target.name()))
            .collect();
        info!("  {:<11} {:>4}  e.g. {}", cat.label(), count, examples.join(", "));
    }

    // Verbose detail: if CARGO_LOG is set in the environment (the same env var
    // cargo's own fingerprint module uses), dump per-unit reasons + the union
    // of every invalidated input we observed across all misses.
    if std::env::var("CARGO_LOG").is_ok() {
        info!("per-unit miss detail:");
        for (unit, cause) in misses {
            let cat = miss_category(cause);
            let trig_count = match cause {
                MissCause::DynamicChanged { diff, .. } => diff.total(),
                MissCause::Cascade { .. } => 1,
                MissCause::NewStaticKey { .. } => 0,
            };
            info!(
                "  miss {} ({}) [{}] triggers={} first={}",
                unit.pkg.name(),
                unit.target.name(),
                cat.label(),
                trig_count,
                first_trigger(cause)
            );
        }

        let mut all_changed_paths = std::collections::BTreeSet::new();
        let mut all_appeared_paths = std::collections::BTreeSet::new();
        let mut all_missing_paths = std::collections::BTreeSet::new();
        let mut all_changed_envs = std::collections::BTreeSet::new();
        for (_, cause) in misses {
            if let MissCause::DynamicChanged { diff, .. } = cause {
                for p in &diff.changed_paths {
                    all_changed_paths.insert(p.clone());
                }
                for p in &diff.appeared_paths {
                    all_appeared_paths.insert(p.clone());
                }
                for p in &diff.missing_paths {
                    all_missing_paths.insert(p.clone());
                }
                for e in &diff.changed_envs {
                    all_changed_envs.insert(e.clone());
                }
            }
        }
        if !all_changed_paths.is_empty() || !all_appeared_paths.is_empty()
            || !all_missing_paths.is_empty() || !all_changed_envs.is_empty() {
            info!("invalidated inputs (union):");
            for p in &all_changed_paths {
                info!("  changed: {}", p.display());
            }
            for p in &all_appeared_paths {
                info!("  appeared: {}", p.display());
            }
            for p in &all_missing_paths {
                info!("  missing: {}", p.display());
            }
            for e in &all_changed_envs {
                info!("  env changed: {e}");
            }
        }
    }
}

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

    // Default keeps our output to a head summary only. -v / -vv / CARGO_LOG
    // expand into our debug/trace paths and (independently) bump cargo's
    // Shell verbosity so its rustc-invocation lines appear.
    let level = if cli.verbose >= 2 {
        "cargo_zb=trace"
    } else if cli.verbose >= 1 || std::env::var_os("CARGO_LOG").is_some() {
        "cargo_zb=debug"
    } else {
        "cargo_zb=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| level.into()),
        )
        .with_target(false)
        .without_time()
        .compact()
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
    debug!("cache: {} at {}", cache.name(), dir.display());
    Ok(cache)
}

fn run_cached_build(cli: &ZbArgs) -> Result<()> {
    let t_start = std::time::Instant::now();
    let cache = open_cache(cli)?;

    let gctx = cargo::GlobalContext::default()?;
    set_cargo_verbosity(&gctx);
    let root = cargo_interop::resolve_manifest(cli.manifest_path.as_deref(), &gctx)?;
    let ws = cargo::core::Workspace::new(&root, &gctx)?;
    let compile_opts = build_compile_options(cli, &gctx)?;

    let interner = UnitInterner::new();
    let bcx = cargo_interop::build_bcx(&ws, &interner, &compile_opts)?;
    let rustc_version = cargo_interop::rustc_verbose_version(&ws)?;
    let target_dir = cargo_interop::target_dir(&ws);
    debug!("unit graph: {} units, {} roots", bcx.unit_graph.len(), bcx.roots.len());

    let static_keys = hash::compute_cache_keys(&bcx.unit_graph, &bcx.roots, &rustc_version)?;
    let t_setup = t_start.elapsed();

    let units: Vec<Unit> = cargo_interop::topo_order(&bcx.unit_graph, &bcx.roots);

    // Phase 1: per-unit lookup in topo order. A unit can hit only if all its
    // deps hit (we need their full_keys to derive ours). For each unit, try
    // every recorded dynamic-inputs manifest under its static_key; pick the
    // first whose (content_hash + dep_full_keys) points to a stored bundle.
    debug!("looking up {} units in cache...", units.len());
    let mut hits: HashMap<Unit, hash::CacheKey> = HashMap::new();
    let mut misses: Vec<(Unit, MissCause)> = Vec::new();

    for unit in &units {
        let static_key = static_keys.get(unit).expect("static key for every unit");

        let dep_units: Vec<&Unit> = bcx
            .unit_graph
            .get(unit)
            .map(|deps| deps.iter().map(|d| &d.unit).collect())
            .unwrap_or_default();
        let missing_dep: Option<&Unit> = dep_units
            .iter()
            .find(|d| !hits.contains_key(*d))
            .copied();

        // Always evaluate own state first — even if a dep is missing — so
        // we report the unit's own root cause instead of hiding it behind a
        // cascade (e.g. on a fresh cold build, every unit's static_key is
        // genuinely new; that's more useful info than "dep X missed").
        let manifests = cache.list_dynamic_inputs(static_key.as_bytes())?;
        let mut best_diff: Option<cache::DiffReport> = None;
        for inputs in &manifests {
            if let Ok(d) = inputs.diff_current(|n| std::env::var(n).ok()) {
                let total = d.total();
                let curr_total = best_diff.as_ref().map(|x| x.total()).unwrap_or(usize::MAX);
                if total < curr_total {
                    best_diff = Some(d);
                }
            }
        }
        // For non-RunCustomBuild units, "env changed" entries from rustc's
        // dep-info may reflect env vars that build scripts set via
        // `cargo:rustc-env=` — those are absent from the process env at
        // zb-invocation time, so `diff_current` flags them even when our
        // content_hash (which also uses process env) is stable. When a dep
        // missed, that's the actual root cause; suppress these false-flag
        // diff entries to avoid mis-classifying the unit as "rust".
        let diff_meaningful = best_diff.as_ref().map(|d| {
            !d.changed_paths.is_empty()
                || !d.appeared_paths.is_empty()
                || !d.missing_paths.is_empty()
                // Only count an env diff as meaningful if its current value
                // is set (suggests a real env change at zb-time). Diffs where
                // current is None but stored was Some(...) are typically
                // build-script-injected envs that never appear in our env.
                || d.changed_envs.iter().any(|n| std::env::var(n).is_ok())
        }).unwrap_or(false);

        let own_would_miss = manifests.is_empty() || diff_meaningful;

        if let Some(dep) = missing_dep {
            // Dep missed. If our own state would have missed too (real diff
            // or no manifest), report own cause; otherwise it's a cascade.
            let cause = if own_would_miss {
                classify_miss(unit, manifests.is_empty(), best_diff)
            } else {
                MissCause::Cascade {
                    dep_name: format!("{} ({})", dep.pkg.name(), dep.target.name()),
                }
            };
            misses.push((unit.clone(), cause));
            continue;
        }

        let dep_full_keys: Vec<hash::CacheKey> = dep_units
            .iter()
            .map(|d| *hits.get(*d).expect("checked above"))
            .collect();

        let mut hit = false;
        for inputs in &manifests {
            let content = match inputs.content_hash(|n| std::env::var(n).ok()) {
                Ok(c) => c,
                Err(e) => {
                    debug!("dynamic content hash failed for {}: {e}", unit.pkg.name());
                    continue;
                }
            };
            let full = hash::combine_full_key(static_key, &content, &dep_full_keys);
            if cache.contains_unit(full.as_bytes())? {
                let restored = artifacts::restore_unit(&*cache, full.as_bytes(), &target_dir)?;
                debug!(
                    "restored {} files for {} ({})",
                    restored,
                    unit.pkg.name(),
                    unit.target.name()
                );
                hits.insert(unit.clone(), full);
                hit = true;
                break;
            }
        }
        if !hit {
            let cause = classify_miss(unit, manifests.is_empty(), best_diff);
            misses.push((unit.clone(), cause));
        }
    }

    let t_lookup = t_start.elapsed() - t_setup;
    print_lookup_summary(&hits, &misses);

    if misses.is_empty() {
        debug!(
            "all units restored from cache (setup={:.2}s lookup={:.2}s)",
            t_setup.as_secs_f64(),
            t_lookup.as_secs_f64(),
        );
        return Ok(());
    }

    // Phase 2: run cargo build. cargo's incremental will treat the restored
    // .fingerprint/ + deps/<crate>-<hash>.rlib state as fresh wherever inputs
    // match, so it should only recompile units whose dynamic inputs we couldn't
    // attest. cargo prints its own status lines (`Compiling X`, `Finished`,
    // any warnings/errors) to stderr — we don't add a banner before it.
    let t_build = std::time::Instant::now();
    cargo_interop::execute_build(&ws, &compile_opts)?;
    let build_secs = t_build.elapsed().as_secs_f64();

    // Phase 3: harvest dynamic inputs + per-unit artifacts in topo order so
    // dep full_keys are available when we compute consumer full_keys. Includes
    // units that hit cache (we need to track their full_key for consumers).
    let t_harvest = std::time::Instant::now();
    debug!("harvesting per-unit cache entries...");
    {
        let runner = cargo_interop::prepared_runner(&bcx)?;
        let mut stored = 0usize;
        let mut skipped = 0usize;
        let mut full_keys: HashMap<Unit, hash::CacheKey> = hits.clone();

        for unit in &units {
            if full_keys.contains_key(unit) {
                continue; // already known from Phase 1 hit
            }
            let static_key = static_keys.get(unit).expect("static key");
            let inputs = match harvest::harvest_unit(&runner, unit)? {
                Some(i) => i,
                None => {
                    skipped += 1;
                    continue;
                }
            };

            let dep_full_keys: Vec<hash::CacheKey> = bcx
                .unit_graph
                .get(unit)
                .map(|deps| {
                    deps.iter()
                        .filter_map(|d| full_keys.get(&d.unit).copied())
                        .collect()
                })
                .unwrap_or_default();

            // If we don't have full_keys for all this unit's deps, don't try
            // to cache it (we'd compute a different key on lookup).
            let dep_count = bcx.unit_graph.get(unit).map(|d| d.len()).unwrap_or(0);
            if dep_full_keys.len() != dep_count {
                debug!(
                    "incomplete dep full_keys for {} ({}); not caching",
                    unit.pkg.name(),
                    unit.target.name()
                );
                skipped += 1;
                continue;
            }

            let content = inputs.content_hash(|n| std::env::var(n).ok())?;
            let full = hash::combine_full_key(static_key, &content, &dep_full_keys);
            full_keys.insert(unit.clone(), full);

            if cache.contains_unit(full.as_bytes())? {
                cache.put_dynamic_inputs(static_key.as_bytes(), &inputs)?;
                continue;
            }

            let unit_artifacts = artifacts::collect_unit_artifacts(&runner, unit);
            if unit_artifacts.files.is_empty() {
                debug!("no artifacts for {} ({})", unit.pkg.name(), unit.target.name());
                continue;
            }

            cache.put_dynamic_inputs(static_key.as_bytes(), &inputs)?;
            let count = artifacts::store_unit(&*cache, full.as_bytes(), &unit_artifacts, &target_dir)?;
            debug!(
                "stored {} files for {} ({})",
                count,
                unit.pkg.name(),
                unit.target.name()
            );
            stored += 1;
        }

        debug!("stored {} unit bundles ({} skipped)", stored, skipped);
    }
    let harvest_secs = t_harvest.elapsed().as_secs_f64();

    debug!(
        "done. setup={:.2}s lookup={:.2}s build={:.2}s harvest={:.2}s total={:.2}s",
        t_setup.as_secs_f64(),
        t_lookup.as_secs_f64(),
        build_secs,
        harvest_secs,
        t_start.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn run_plain_build(cli: &ZbArgs) -> Result<()> {
    let gctx = cargo::GlobalContext::default()?;
    set_cargo_verbosity(&gctx);
    let root = cargo_interop::resolve_manifest(cli.manifest_path.as_deref(), &gctx)?;
    let ws = cargo::core::Workspace::new(&root, &gctx)?;
    let compile_opts = build_compile_options(cli, &gctx)?;
    cargo_interop::execute_build(&ws, &compile_opts)
}

/// Cargo's `Shell::new()` defaults to `Verbose`, which causes every rustc
/// invocation to be echoed. The cargo CLI explicitly downgrades this in
/// `configure()` to match user expectations. We do the same: Normal by default,
/// Verbose only if `CARGO_LOG` is set in the environment.
fn set_cargo_verbosity(gctx: &cargo::GlobalContext) {
    use cargo::core::shell::Verbosity;
    let verbosity = if std::env::var_os("CARGO_LOG").is_some() {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    gctx.shell().set_verbosity(verbosity);
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
