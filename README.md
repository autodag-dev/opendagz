# OpenDagz

Open source tools from [Dagz](https://dagz.run) for CI observability and build acceleration.

## Tools

### [`zb time`](cmd/) - Process tree profiler

A `time(1)` replacement that traces the entire process tree with per-process CPU, memory, I/O, and thread metrics. Useful for understanding complex build systems.

**Linux only.** macOS in progress.

### [`cargo-zb`](cargo-zb/) - Cargo build cache

A drop-in cargo wrapper that caches entire build outputs. On cache hit, restores all artifacts via `copy_file_range(2)` — no recompilation, no per-crate overhead.

**Up to 35x faster than sccache** on cache-hit rebuilds. See [cargo-zb/README.md](cargo-zb/) for benchmarks.

## Installation

### Download

[Latest release](https://github.com/autodag-dev/opendagz/releases)

### Install via Cargo

```bash
cargo install opendagz        # zb time
cargo install cargo-zb         # cargo-zb
```

## Requirements

- **Linux** (x86_64). Uses `ptrace(2)` for process tracing and `copy_file_range(2)` for zero-copy file operations.
- Modern kernel with [schedstat](https://docs.kernel.org/scheduler/sched-stats.html) for accurate CPU metrics (most distros have this).

## License

GNU Affero General Public License v3.0 (AGPL-3.0).

For commercial licensing, contact rnd@dagz.run.

## Support

- Issues: [GitHub Issues](https://github.com/autodag-dev/opendagz/issues)
- Discussions: [GitHub Discussions](https://github.com/autodag-dev/opendagz/discussions)
