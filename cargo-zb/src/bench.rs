use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunMetrics {
    pub label: String,
    pub wall_secs: f64,
    pub user_secs: f64,
    pub sys_secs: f64,
    pub peak_rss_kb: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BenchResult {
    pub project_name: String,
    pub manifest_path: String,
    pub timestamp: String,
    pub rustc_version: String,
    pub artifact_count: u64,
    pub artifact_total_bytes: u64,
    pub runs: Vec<RunMetrics>,
}

fn bench_db_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(Path::new(&home).join(".cache").join("cargo-zb-bench").join("results.json"))
}

fn load_results() -> Result<Vec<BenchResult>> {
    let path = bench_db_path()?;
    match std::fs::read(&path) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).context("reading bench results"),
    }
}

fn save_result(result: &BenchResult) -> Result<()> {
    let path = bench_db_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut results = load_results()?;
    if let Some(pos) = results.iter().position(|r| r.project_name == result.project_name) {
        results[pos] = result.clone();
    } else {
        results.push(result.clone());
    }
    let data = serde_json::to_vec_pretty(&results)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn timed_run(label: &str, cmd: &str, args: &[&str], dir: &Path, env: &[(&str, &str)]) -> Result<RunMetrics> {
    eprintln!("\n--- {label} ---");
    eprintln!("  $ {} {}", cmd, args.join(" "));

    let start = Instant::now();
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().cloned())
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {cmd}"))?;

    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    let mut rusage: libc::rusage = unsafe { std::mem::zeroed() };

    let wait_result = unsafe {
        libc::wait4(pid, &mut status, 0, &mut rusage)
    };
    let wall = start.elapsed().as_secs_f64();

    if wait_result < 0 {
        // Fallback: just wait normally
        let out = child.wait()?;
        let wall = start.elapsed().as_secs_f64();
        if !out.success() {
            anyhow::bail!("{label}: command exited with {out}");
        }
        return Ok(RunMetrics {
            label: label.to_string(),
            wall_secs: wall,
            user_secs: 0.0,
            sys_secs: 0.0,
            peak_rss_kb: 0,
        });
    }

    let user_secs = rusage.ru_utime.tv_sec as f64 + rusage.ru_utime.tv_usec as f64 / 1_000_000.0;
    let sys_secs = rusage.ru_stime.tv_sec as f64 + rusage.ru_stime.tv_usec as f64 / 1_000_000.0;
    let peak_rss_kb = rusage.ru_maxrss as u64; // Linux: already in KB

    let exit_code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };

    eprintln!("  wall: {wall:.2}s  user: {user_secs:.2}s  sys: {sys_secs:.2}s  rss: {peak_rss_kb} KB  exit: {exit_code}");

    if exit_code != 0 {
        anyhow::bail!("{label}: command exited with code {exit_code}");
    }

    Ok(RunMetrics {
        label: label.to_string(),
        wall_secs: wall,
        user_secs,
        sys_secs,
        peak_rss_kb,
    })
}

fn drop_caches() -> bool {
    let _ = std::process::Command::new("sync").status();
    let result = std::fs::write("/proc/sys/vm/drop_caches", "3");
    if result.is_err() {
        eprintln!("  warning: cannot drop_caches (need root). Cold numbers will be warm.");
        return false;
    }
    true
}

fn count_dir(dir: &Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if !dir.exists() {
        return (0, 0);
    }
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            count += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (count, bytes)
}

fn detect_project_name(manifest_path: &Path) -> String {
    if let Ok(contents) = std::fs::read_to_string(manifest_path) {
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with("name") {
                if let Some(val) = line.split('=').nth(1) {
                    let val = val.trim().trim_matches('"');
                    return val.to_string();
                }
            }
        }
    }
    manifest_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn self_exe() -> Result<PathBuf> {
    std::env::current_exe().context("getting current exe path")
}

pub fn run_bench(
    manifest_path: &Path,
    release: bool,
    cache_backend: &str,
    with_sccache: bool,
) -> Result<()> {
    let manifest_path = manifest_path.canonicalize()
        .with_context(|| format!("canonicalizing {}", manifest_path.display()))?;
    let project_dir = manifest_path.parent().unwrap();
    let project_name = detect_project_name(&manifest_path);

    let rustc_version = {
        let out = std::process::Command::new("rustc").arg("--version").output()?;
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    eprintln!("=== Benchmarking: {project_name} ===");
    eprintln!("  manifest: {}", manifest_path.display());
    eprintln!("  rustc: {rustc_version}");
    eprintln!("  backend: {cache_backend}");
    if with_sccache {
        eprintln!("  sccache: enabled");
    }

    let mut runs = Vec::new();
    let cargo_build_args: Vec<&str> = if release {
        vec!["build", "--release"]
    } else {
        vec!["build"]
    };

    let target_dir = resolve_target_dir(&manifest_path)?;

    eprintln!("\n=== Step 1: Clean target + cache ===");
    let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);

    let cache_dir = crate::cache::default_cache_dir()?;
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::fs::create_dir_all(&cache_dir)?;

    eprintln!("\n=== Step 2: Baseline cargo build ===");
    let baseline = timed_run("baseline (no cache)", "cargo", &cargo_build_args, project_dir, &[])?;
    runs.push(baseline);

    let (artifact_count, artifact_total_bytes) = count_dir(&target_dir);
    eprintln!("  artifacts: {artifact_count} files, {:.1} MiB", artifact_total_bytes as f64 / (1024.0 * 1024.0));

    eprintln!("\n=== Step 3: Populate cargo-zb cache ===");

    let zb_exe = self_exe()?;
    let zb_exe_str = zb_exe.to_string_lossy().to_string();
    let manifest_str = manifest_path.to_string_lossy().to_string();
    let mut zb_args: Vec<&str> = vec![
        "zb",
        "--manifest-path", &manifest_str,
        "--cache-backend", cache_backend,
    ];
    if release {
        zb_args.push("--release");
    }

    let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::fs::create_dir_all(&cache_dir)?;

    let store = timed_run("cargo-zb first build", &zb_exe_str, &zb_args, project_dir, &[])?;
    runs.push(store);

    eprintln!("\n=== Step 4: Cold cache restore ===");
    let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);
    let cold_ok = drop_caches();

    let cold_label = if cold_ok { "cargo-zb restore (cold)" } else { "cargo-zb restore (warm*)" };
    let cold_restore = timed_run(cold_label, &zb_exe_str, &zb_args, project_dir, &[])?;
    runs.push(cold_restore);

    eprintln!("\n=== Step 5: Warm cache restore ===");
    let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);
    let warm_restore = timed_run("cargo-zb restore (warm)", &zb_exe_str, &zb_args, project_dir, &[])?;
    runs.push(warm_restore);

    if with_sccache {
        if std::process::Command::new("sccache").arg("--version").output().is_err() {
            eprintln!("\n  warning: sccache not found in PATH. Install it for comparison benchmarks.");
            eprintln!("  skipping sccache steps.");
        } else {
            let sccache_dir = cache_dir.join("sccache-bench");

            let _ = std::process::Command::new("sccache")
                .arg("--stop-server")
                .output();

            eprintln!("\n=== Step 6: sccache cold build ===");
            let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);
            let _ = std::fs::remove_dir_all(&sccache_dir);

            let sccache_dir_str = sccache_dir.to_string_lossy().to_string();
            let sccache_env = [
                ("RUSTC_WRAPPER", "sccache"),
                ("SCCACHE_DIR", sccache_dir_str.as_str()),
            ];
            match timed_run("sccache cold (miss)", "cargo", &cargo_build_args, project_dir, &sccache_env) {
                Ok(m) => runs.push(m),
                Err(e) => eprintln!("  sccache cold build failed: {e}"),
            }

            eprintln!("\n=== Step 7: sccache warm build ===");
            let _ = timed_run("cargo clean", "cargo", &["clean"], project_dir, &[]);
            match timed_run("sccache warm (hit)", "cargo", &cargo_build_args, project_dir, &sccache_env) {
                Ok(m) => runs.push(m),
                Err(e) => eprintln!("  sccache warm build failed: {e}"),
            }

            let _ = std::process::Command::new("sccache")
                .arg("--show-stats")
                .env("SCCACHE_DIR", &sccache_dir_str)
                .status();

            let _ = std::process::Command::new("sccache")
                .arg("--stop-server")
                .output();
            let _ = std::fs::remove_dir_all(&sccache_dir);
        }
    }

    let result = BenchResult {
        project_name: project_name.clone(),
        manifest_path: manifest_path.to_string_lossy().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        rustc_version,
        artifact_count,
        artifact_total_bytes,
        runs,
    };

    save_result(&result)?;
    eprintln!();
    print_one_result(&result);

    Ok(())
}

fn print_one_result(r: &BenchResult) {
    let size_mib = r.artifact_total_bytes as f64 / (1024.0 * 1024.0);
    println!("{} ({} files, {:.1} MiB)", r.project_name, r.artifact_count, size_mib);
    println!("  rustc: {}", r.rustc_version);
    println!("  {}", r.timestamp);
    println!();
    println!("  {:<30} {:>8} {:>8} {:>8} {:>10}",
        "step", "wall", "user", "sys", "peak RSS");
    println!("  {}", "-".repeat(68));
    for run in &r.runs {
        let rss = if run.peak_rss_kb > 0 {
            format_size(run.peak_rss_kb * 1024)
        } else {
            "-".to_string()
        };
        println!("  {:<30} {:>7.2}s {:>7.2}s {:>7.2}s {:>10}",
            run.label, run.wall_secs, run.user_secs, run.sys_secs, rss);
    }
    println!();
}

pub fn list_benched() -> Result<()> {
    let results = load_results()?;
    if results.is_empty() {
        println!("No benchmarks stored yet. Run `cargo-zb bench` first.");
        return Ok(());
    }

    println!();
    println!("cargo-zb benchmark results");
    println!("stored at: {}", bench_db_path()?.display());
    println!();

    let has_sccache = results.iter().any(|r| r.runs.iter().any(|m| m.label.contains("sccache")));
    if has_sccache {
        println!("{:<20} {:>6} {:>8} {:>10} {:>10} {:>10} {:>10}",
            "project", "files", "size", "baseline", "sccache", "cargo-zb", "speedup");
        println!("{}", "-".repeat(78));
    } else {
        println!("{:<20} {:>6} {:>8} {:>10} {:>10} {:>10}",
            "project", "files", "size", "baseline", "store", "restore");
        println!("{}", "-".repeat(68));
    }

    for r in &results {
        let size = format_size(r.artifact_total_bytes);
        let baseline = find_run(&r.runs, "baseline").map(|m| format!("{:.1}s", m.wall_secs)).unwrap_or("-".into());
        let warm_val = r.runs.iter().rfind(|m| m.label.contains("restore") && m.label.contains("warm") && !m.label.contains("*"))
            .map(|m| m.wall_secs);
        let sccache_warm = r.runs.iter().find(|m| m.label.contains("sccache") && m.label.contains("warm"))
            .map(|m| m.wall_secs);

        if has_sccache {
            let sc = sccache_warm.map(|s| format!("{:.1}s", s)).unwrap_or("-".into());
            let zb = warm_val.map(|s| format!("{:.1}s", s)).unwrap_or("-".into());
            let speedup = match (sccache_warm, warm_val) {
                (Some(s), Some(z)) if z > 0.0 => format!("{:.0}x", s / z),
                _ => "-".into(),
            };
            println!("{:<20} {:>6} {:>8} {:>10} {:>10} {:>10} {:>10}",
                truncate(&r.project_name, 20), r.artifact_count, size, baseline, sc, zb, speedup);
        } else {
            let store = r.runs.iter().find(|m| m.label.contains("cargo-zb") && m.label.contains("first"))
                .map(|m| format!("{:.1}s", m.wall_secs)).unwrap_or("-".into());
            let restore = warm_val.map(|s| format!("{:.1}s", s)).unwrap_or("-".into());
            println!("{:<20} {:>6} {:>8} {:>10} {:>10} {:>10}",
                truncate(&r.project_name, 20), r.artifact_count, size, baseline, store, restore);
        }
    }

    for r in &results {
        println!();
        print_one_result(r);

        let sccache_warm = r.runs.iter().find(|m| m.label.contains("sccache") && m.label.contains("warm"));
        let zb_warm = r.runs.iter().rfind(|m| m.label.contains("restore") && m.label.contains("warm") && !m.label.contains("*"));
        if let (Some(sc), Some(zb)) = (sccache_warm, zb_warm) {
            if zb.wall_secs > 0.0 {
                let speedup = sc.wall_secs / zb.wall_secs;
                println!("  cargo-zb is {speedup:.1}x faster than sccache on warm restore");
            }
        }
    }

    println!();
    Ok(())
}

fn find_run<'a>(runs: &'a [RunMetrics], keyword: &str) -> Option<&'a RunMetrics> {
    runs.iter().find(|m| m.label.to_lowercase().contains(keyword))
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn resolve_target_dir(manifest_path: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        anyhow::bail!("cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("parsing cargo metadata")?;
    let target_dir = meta["target_directory"]
        .as_str()
        .context("missing target_directory in cargo metadata")?;
    Ok(PathBuf::from(target_dir))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
