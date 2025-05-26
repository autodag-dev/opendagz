# cargo-zb

Cargo build cache that operates on entire builds, not individual crates.

On cache hit, `cargo-zb` skips compilation entirely and restores all artifacts via Linux `copy_file_range(2)` — zero userspace copies, sub-second rebuilds.

## Benchmarks

Cache-hit restore time vs sccache (`cargo zb bench`):

| Project | Files | Size | Baseline | sccache | cargo-zb | Speedup |
|---------|------:|-----:|---------:|--------:|---------:|--------:|
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 335 | 213 MiB | 17.2s | 6.3s | **0.17s** | **37x** |
| [reqwest](https://github.com/seanmonstar/reqwest) (rustls + aws-lc) | 1257 | 222 MiB | 37.9s | 6.4s | **0.23s** | **28x** |
| cargo-zb (self-build, 424 units) | 3442 | 893 MiB | 199s | — | **0.42s** | — |

*Measured on NVMe (SK Hynix, 3.9 GB/s), rustc 1.94.0, 16 cores. Warm page cache, fs backend, 4 IO threads.
Run your own: `cargo zb --release --manifest-path path/to/Cargo.toml bench`*

### Why it's fast

- **Whole-build caching.** sccache wraps each `rustc` invocation (process spawn + tokio runtime + daemon RPC per crate). cargo-zb hashes the entire unit graph upfront and does a single cache lookup.
- **Zero-copy restore.** Uses `copy_file_range(2)` to write cached files directly from page cache to disk — no read-into-buffer, no write-from-buffer.
- **No binary gap.** sccache doesn't cache binary crates. For ripgrep, the `rg` binary link alone is 3s — uncacheable. cargo-zb caches everything.
- **Cargo as a library.** No wrapper processes. Resolves the unit graph and computes cache keys in-process via `cargo = "0.94"`.

### Detailed breakdown (ripgrep)

```
step                               wall     user      sys   peak RSS
--------------------------------------------------------------------
baseline (no cache)              17.2s   152.3s    7.3s  516.2 MiB
cargo-zb first build             17.0s   144.7s    6.4s  499.2 MiB
cargo-zb restore (warm)           0.2s     0.1s    0.1s   34.0 MiB
sccache cold (miss)              20.8s    35.2s    1.9s  514.2 MiB
sccache warm (hit)                6.3s    35.2s    1.8s  519.9 MiB
```

## Install

```bash
cargo install --path cargo-zb
```

## Usage

```bash
# Build with caching (use just like cargo build)
cargo zb --release

# Build a specific project
cargo zb --release --manifest-path /path/to/Cargo.toml

# Use LMDB backend instead of filesystem
cargo zb --cache-backend lmdb

# Run benchmarks (includes sccache comparison if installed)
cargo zb bench --release --manifest-path /path/to/Cargo.toml

# Show all stored benchmark results
cargo zb list-benched
```

## How it works

1. **Plan** — resolve the workspace and compute the full unit graph using cargo as a library.
2. **Hash** — compute a blake3 content hash for each compilation unit (rustc version, profile, features, rustflags, source files, dependency hashes). Derive a single build key from all unit keys.
3. **Lookup** — check if the build key exists in the cache.
4. **Hit** — restore all artifact files to `target/` via `copy_file_range`. Done.
5. **Miss** — snapshot `target/`, run the build, diff to find new/modified files, store them in the cache.

Cache keys are content-based (no mtimes). Registry/git deps are keyed by version/commit. Path deps are keyed by source file contents.

## Cache backends

- **fs** (default) — one file per artifact under `~/.cache/cargo-zb/`. Uses `copy_file_range(2)` for zero-copy on both store and restore. Parallel reads scale well on NVMe.
- **lmdb** — all artifacts in a single LMDB database. Zero-copy mmap reads, batched writes. Slower than fs for large builds due to fsync overhead.

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--cache-backend` | `fs` | `fs` or `lmdb` |
| `--cache-dir` | `~/.cache/cargo-zb/` | Cache directory |
| `--io-threads` | `4` | Parallel threads for cache restore |
| `--release` | off | Build in release mode |
| `--no-cache` | off | Skip caching, just run `cargo build` |

Environment: `CARGO_ZB_CACHE_DIR` overrides the default cache directory.

## Requirements

- **Linux** (x86_64)
- Rust toolchain managed by rustup
- For benchmark sccache comparison: sccache installed (optional, use `--no-sccache` to skip)
