# sccache Performance Analysis

Investigation of sccache bottlenecks to inform the design of cargo-zb.

**Test subject:** ripgrep (medium Rust project, 33 cacheable compilation units)
**Toolchain:** rustc 1.94.0, cargo 1.94.0, sccache 0.12.0
**Platform:** Linux x86_64, 16 cores

## Build Time Benchmarks

| Scenario | Wall time | CPU time | Notes |
|----------|-----------|----------|-------|
| Baseline `cargo build --release` | 12.5s | 122s | No caching |
| sccache cold (all misses) | 13.6s | 29.7s | +1.1s overhead vs baseline |
| sccache warm (all hits, -j auto) | 3.8s | 19.4s | Parallelism hides per-unit overhead |
| sccache warm (all hits, -j1) | 6.9s | 19.0s | Exposes ~4.9s sequential overhead |
| Passthrough wrapper (no caching) | 9.0s | 83.7s | Shell wrapper overhead is negligible |

The warm-cache build burns 19.4s of CPU time across 3.8s wall time -- about 570ms CPU per compilation unit just for cache restore.

## sccache Stats

```
Compile requests                     46
Compile requests executed            34
Cache hits                           33
Cache misses                          0
Non-cacheable calls                  11
  - crate-type (binaries)            16
  - missing input                     4
```

Key: **sccache doesn't cache binaries.** The `rg` binary link alone takes 2.97s and is never cached. This is 75% of the warm-cache wall time.

## Per-Invocation Timing (wrapper_times.log)

Measured by wrapping sccache with a timing shell script on a warm-cache `-j1` build:

```
18ms unknown          141ms build_script_build
19ms ___              146ms itoa
17ms unknown          147ms build_script_build
58ms log              149ms memchr
65ms cfg_if           147ms build_script_build
68ms same_file         90ms walkdir
62ms lexopt           173ms build_script_build
83ms termcolor        100ms textwrap
88ms ryu              176ms build_script_build
                      194ms regex_syntax
107ms crossbeam_utils 200ms build_script_build
140ms aho_corasick    218ms build_script_build
242ms encoding_rs      74ms grep_matcher
82ms crossbeam_epoch  225ms regex_automata
51ms encoding_rs_io   127ms serde_json
75ms anyhow           109ms bstr
197ms serde_core       70ms grep_regex
61ms crossbeam_deque   77ms globset
179ms libc             78ms grep_searcher
102ms serde            85ms grep_cli
68ms memmap2           98ms grep_printer
                      101ms ignore
                       55ms grep
                     2975ms rg (binary, NOT CACHED)
```

- Small crates (cfg_if, log): ~60ms overhead per cache hit
- Medium crates (memchr, serde): ~100-200ms per cache hit
- Large crates (encoding_rs, regex_automata): ~225-242ms per cache hit
- **rg binary: 2975ms -- not cached at all**

## Syscall Analysis (strace)

### Per-invocation syscall summary (single memchr cache hit)

From `strace -f -T -c` on a single sccache invocation:

```
% time     seconds  usecs/call     calls  syscall
------ ----------- ----------- --------- ------------------
 65.20    0.051535         780        66  futex
  8.42    0.006653         246        27  sched_yield
  5.53    0.004373        4373         1  epoll_wait
  2.57    0.002032          46        44  mprotect
  1.66    0.001314          77        17  clone3
  1.07    0.000845         422         2  recvfrom
  0.26    0.000203         101         2  sendto
  0.21    0.000166          10        16  statx
```

**65% of time in `futex`** (thread synchronization). Only 16 `statx` calls and 36 reads -- file I/O is negligible. The bottleneck is thread management.

### Detailed timeline (strace -f -T on memchr cache hit, ~149ms total)

| Phase | Time | What happens |
|-------|------|-------------|
| `execve` + dynamic linking | ~2ms | Links libssl, libcrypto, libzstd, libz |
| cgroup probing | ~1ms | Reads `/sys/fs/cgroup` hierarchy twice |
| **Tokio thread pool spawn** | **~3ms** | **17 `clone3` calls -- full async runtime** |
| Futex cascade (thread init) | ~1.2ms | Threads synchronize via futex |
| `sendto` to daemon | 0.13ms | Sends 8KB of rustc args to daemon |
| **`recvfrom` from daemon** | **102ms** | **Daemon: hash + lookup + read = 68% of total** |
| Write artifact JSON to stderr | 0.02ms | 3 small writes |
| Thread shutdown + join | ~4ms | Wake and join all 17 threads |

## Root Causes

### 1. Daemon processing: 102ms per cache hit (68% of invocation time)

The sccache daemon re-hashes source files and dependency artifacts on every cache lookup. For memchr, the client sends 8KB of rustc arguments, then blocks for 102ms while the daemon:
- Computes the cache key (hashing source files + deps)
- Does the cache lookup on disk
- Reads and returns the cached artifact

### 2. Tokio thread pool: ~8ms per invocation

sccache spawns **17 threads per invocation** via `clone3` -- a full tokio async runtime with thread pool. The actual work is essentially synchronous (send request, wait for response). These threads are created and torn down for every compilation unit.

### 3. No binary caching

sccache marks binary crate types as "non-cacheable" (16 of 46 compile requests). The `rg` binary link takes 2.97s -- 75% of the warm-cache wall time.

### 4. No batch mode

Each of 46 rustc invocations independently:
- Spawns a process
- Links shared libraries (libssl, libcrypto, libzstd)
- Creates 17 threads
- Connects to daemon via TCP
- Sends args, waits for response
- Tears down threads

There's no way to batch cache lookups or pre-populate the target directory.

## Implications for cargo-zb

| sccache problem | cargo-zb solution |
|----------------|-------------------|
| 102ms daemon hash per hit | Compute cache keys once from unit graph metadata; no re-hashing |
| 17-thread tokio runtime per invocation | Single process, no async runtime needed |
| Binary crate type not cached | Cache everything: bins, build scripts, proc macros |
| No batch mode | Resolve full unit graph, bulk cache lookup, restore all hits at once |
| Dynamic linking overhead (libssl etc.) | Static binary, no TLS libs needed for local cache |
| Per-unit process spawn | No wrapper processes; use cargo as a library |

## Profiling Method

### Tools used
- `time_wrapper.sh` -- shell wrapper around sccache that logs per-invocation wall time
- `strace -f -T -c` -- syscall summary (counts + time per syscall)
- `strace -f -T -e trace=clone3,connect,sendto,recvfrom,execve,openat,read,write,futex` -- detailed timeline
- `sccache --show-stats` -- sccache's own statistics

### Raw data files
- `wrapper_times.log` -- per-invocation timing (crate name + ms)
- `strace_sccache.log` -- syscall summary for memchr cache hit
- `strace_detailed.log` -- full syscall timeline for memchr (323 lines)
- `strace_wrapper.sh`, `strace_wrapper2.sh` -- the strace wrapper scripts
- `time_wrapper.sh` -- the timing wrapper script
