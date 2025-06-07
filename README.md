# OpenDagz

Welcome to OpenDagz! 🚀

OpenDagz is the open source project from Dagz, providing tools and libraries for CI observability and acceleration.

## Tools

### `zb time`

A `time(1)` replacement that prints the entire command tree with performance metrics. Useful for analyzing complex build systems.

**Platform Support:**
- ✅ Linux (supported)
- 🚧 macOS (in progress)

## Installation

### Download

Download the [latest release here](https://github.com/autodag-dev/opendagz/releases).

### Install via Cargo

```bash
cargo install opendagz
```

## Example Usage

### Analyze CPython build
Compiling [CPython](https://github.com/python/cpython) with `zb time` shows 3 end sections:
1. Command tree with performance metrics for each command.
   
   The commands are truncated to fit in the terminal. Use `--output FILE` to save the full commands to a file.
2. Group by command, ordered by the most CPU-intensive commands.
3. Summary line, similar to `time(1)` output.


```
$ zb time make -j10
...
    ├─#935        00:00:23.616      0.365s     0.6%cpu (tree:    96.1%cpu)   42 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ ├─#936      00:00:23.617      0.342s    98.0%cpu (tree:    98.0%cpu)   42 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    │ └─#937      00:00:23.963      0.017s    76.6%cpu (tree:    76.6%cpu)    5 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
    ├─#938        00:00:23.993      0.040s     5.4%cpu (tree:    66.3%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/fcntlmodule.o -L/usr/li
    │ └─#939      00:00:23.996      0.034s     4.8%cpu (tree:    71.9%cpu)    9 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
    │   └─#940    00:00:23.999      0.030s    74.5%cpu (tree:    74.5%cpu)    9 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
    ├─#941        00:00:24.039      3.560s     0.2%cpu (tree:    99.0%cpu)   90 MB     0+7k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ ├─#942      00:00:24.048      3.429s    99.2%cpu (tree:    99.2%cpu)   90 MB     0+5k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    │ └─#943      00:00:27.478      0.119s    97.9%cpu (tree:    97.9%cpu)   27 MB     0+1k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
    ├─#944        00:00:27.604      0.044s     3.9%cpu (tree:    97.5%cpu)   16 MB     0+1k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/_testcapimodule.o -L/us
    │ └─#945      00:00:27.606      0.042s     2.7%cpu (tree:    98.0%cpu)   11 MB     0+1k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
    │   └─#946    00:00:27.607      0.041s    98.1%cpu (tree:    98.1%cpu)   11 MB     0+1k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
    ├─#947        00:00:23.612      0.318s     0.9%cpu (tree:    94.8%cpu)   41 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ ├─#948      00:00:23.614      0.294s    98.1%cpu (tree:    98.1%cpu)   41 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    │ └─#949      00:00:23.912      0.017s    63.6%cpu (tree:    63.6%cpu)    5 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
    ├─#950        00:00:23.938      0.029s     8.1%cpu (tree:    76.4%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/grpmodule.o -L/usr/lib/
    │ └─#951      00:00:23.943      0.024s    23.4%cpu (tree:    83.0%cpu)    9 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
    │   └─#952    00:00:23.951      0.015s    93.8%cpu (tree:    93.8%cpu)    9 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
    ├─#953        00:00:23.976      2.240s     0.2%cpu (tree:    93.7%cpu)   70 MB     0+3k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ ├─#954      00:00:23.980      2.106s    94.7%cpu (tree:    94.7%cpu)   70 MB     0+2k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    │ └─#955      00:00:26.087      0.124s    83.0%cpu (tree:    83.0%cpu)   15 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
    ├─#956        00:00:26.223      0.038s     4.7%cpu (tree:    76.7%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/audioop.o -L/usr/lib/x8
    │ └─#957      00:00:26.228      0.031s     3.8%cpu (tree:    89.5%cpu)   10 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
    │   └─#958    00:00:26.229      0.029s    89.9%cpu (tree:    89.9%cpu)   10 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
    ├─#959        00:00:26.266      0.180s     4.4%cpu (tree:    95.4%cpu)   37 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ └─#960      00:00:26.268      0.170s    96.1%cpu (tree:    96.1%cpu)   37 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    ├─#961        00:00:26.455      0.032s    10.1%cpu (tree:    81.5%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/xxlimited_35.o -L/usr/l
    │ └─#962      00:00:26.461      0.026s    29.1%cpu (tree:    89.7%cpu)    9 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
    │   └─#963    00:00:26.468      0.017s    90.9%cpu (tree:    90.9%cpu)    9 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
    ├─#964        00:00:23.620      4.051s     0.1%cpu (tree:    97.5%cpu)   74 MB     0+5k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
    │ ├─#965      00:00:23.622      3.953s    97.5%cpu (tree:    97.5%cpu)   74 MB     0+4k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
    │ └─#966      00:00:27.576      0.094s    97.9%cpu (tree:    97.9%cpu)   19 MB     0+1k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
    └─#967        00:00:27.675      0.031s     5.2%cpu (tree:    96.3%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/socketmodule.o -L/usr/l
      └─#968      00:00:27.677      0.030s     4.1%cpu (tree:    96.5%cpu)   10 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
        └─#969    00:00:27.678      0.028s    96.7%cpu (tree:    96.7%cpu)   10 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g

Group by command (most cpu-intensive last):
    0.001s    66.8%cpu (tree:    66.8%cpu)    1 MB avg    1 MB max  0+0k execs  cat
    0.001s    86.1%cpu (tree:    86.1%cpu)    2 MB avg    2 MB max  0+0k execs  /usr/bin/mkdir
    0.001s    89.5%cpu (tree:    89.5%cpu)    2 MB avg    2 MB max  0+0k execs  sed
    0.001s    44.1%cpu (tree:    82.0%cpu)   20 MB avg   20 MB max  0+0k execs  sh 
    0.002s    79.4%cpu (tree:    79.4%cpu)    6 MB avg    7 MB max  0+0k execs  rm
    0.010s    94.3%cpu (tree:    94.3%cpu)    8 MB avg    8 MB max  0+0k execs  ./python import
    0.010s     0.1%cpu (tree:  1125.9%cpu)   30 MB avg  117 MB max 0+203k execs  /bin/sh 
    0.029s   119.3%cpu (tree:   119.3%cpu)   20 MB avg   51 MB max  0+0k execs  git
    0.041s    97.2%cpu (tree:    97.2%cpu)    7 MB avg    8 MB max  0+1k execs  ./Programs/_freeze_module
    0.101s    98.4%cpu (tree:    98.4%cpu)   17 MB avg   17 MB max  0+1k execs  ./python sysconfig
    0.133s    88.6%cpu (tree:    88.6%cpu)   55 MB avg   59 MB max 0+199k execs  ar
    0.181s     5.2%cpu (tree:    79.3%cpu)   13 MB avg   75 MB max 0+204k execs  /usr/libexec/gcc/x86_64-linux-gnu/13/collect2
    0.217s     0.7%cpu (tree:   896.5%cpu)  312 MB avg  312 MB max 0+1079k execs  make
    1.119s    16.4%cpu (tree:  1165.7%cpu)  117 MB avg  117 MB max 0+202k execs  ./python ./setup.py
    1.356s     0.7%cpu (tree:    97.7%cpu)   57 MB avg  312 MB max 0+652k execs  gcc
    2.231s     2.7%cpu (tree:    93.5%cpu)   36 MB avg  117 MB max 0+198k execs  /usr/bin/gcc
    2.639s    81.1%cpu (tree:    81.1%cpu)   13 MB avg   75 MB max 0+204k execs  /usr/bin/ld
    4.071s    95.9%cpu (tree:    95.9%cpu)   13 MB avg   83 MB max 0+23k execs  ./_bootstrap_python
   13.274s    92.5%cpu (tree:    92.5%cpu)   11 MB avg  148 MB max 0+135k execs  as
  245.634s    97.2%cpu (tree:    97.2%cpu)   56 MB avg  312 MB max 0+503k execs  /usr/libexec/gcc/x86_64-linux-gnu/13/cc1

make: 969 commands  30.233s   896.5%cpu      0+1079k iops      1 PF  Exited 0
```


## Requirements

**For `zb time`:**
- **Process Tracking**: Uses `ptrace(2)` to monitor child processes and resource usage. This works in most environments, but if you encounter permission errors, check the [ptrace(2) man page](https://man7.org/linux/man-pages/man2/ptrace.2.html) for troubleshooting.
- **Precise CPU Metrics**: Requires a modern Linux kernel with [schedstat](https://docs.kernel.org/scheduler/sched-stats.html) enabled for the most accurate CPU measurements (available on most modern Linux distributions).

## Contributing

We welcome contributions from the community! Whether you're fixing bugs, adding features, or improving documentation, your help makes OpenDagz better for everyone.

## License
This project is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).

For commercial use or if you need a more permissive license, please contact us at rnd@dagz.run.


## Support

- 🐛 Issues: [GitHub Issues](https://github.com/autodag-dev/opendagz/issues)
- 💬 Discussions: [GitHub Discussions](https://github.com/autodag-dev/opendagz/discussions)

---

Built with ❤️ by the Dagz team and contributors
