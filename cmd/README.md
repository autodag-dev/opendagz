# zb time

A `time(1)` replacement that traces the entire process tree, reporting per-process wall time, CPU usage, memory, I/O, thread count, and exit code.

## Usage

```bash
zb time <command> [args...]
zb time --output trace.txt make -j16
```

## What it shows

Three sections at the end of every run:

1. **Process tree** — every spawned process with its metrics, nested by parent/child relationships.
2. **Group by command** — aggregated stats per executable, sorted by CPU time.
3. **Summary line** — total wall time, CPU utilization, I/O, like `time(1)`.

## Example: CPython build

```
$ zb time make -j10
...
    ├─#935        00:00:23.616      0.365s     0.6%cpu (tree:    96.1%cpu)   42 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC ...
    │ ├─#936      00:00:23.617      0.342s    98.0%cpu (tree:    98.0%cpu)   42 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/.../cc1 -quiet ...
    │ └─#937      00:00:23.963      0.017s    76.6%cpu (tree:    76.6%cpu)    5 MB     0+0k iops    0 PF    1 threads   [rc=0] as ...
    ...

Group by command (most cpu-intensive last):
    0.002s    79.4%cpu     6 MB avg  rm
    0.133s    88.6%cpu    55 MB avg  ar
    2.639s    81.1%cpu    13 MB avg  /usr/bin/ld
   13.274s    92.5%cpu    11 MB avg  as
  245.634s    97.2%cpu    56 MB avg  /usr/libexec/gcc/.../cc1

make: 969 commands  30.233s   896.5%cpu      0+1079k iops      1 PF  Exited 0
```

## How it works

Uses `ptrace(2)` to intercept `fork`/`clone`/`exec`/`exit` events across the entire process tree. Collects `getrusage` data and reads `/proc/[pid]/schedstat` for precise CPU accounting (run time vs. wait time).

No LD_PRELOAD, no wrappers injected into the build — just ptrace from the outside.

## Requirements

- **Linux** (x86_64)
- Kernel with schedstat enabled (most modern distros)
- `ptrace` permissions (works by default; check `/proc/sys/kernel/yama/ptrace_scope` if needed)

## Platform

- Linux: supported
- macOS: in progress
