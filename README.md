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

## Examples

### Analyzing CPython build

```
$ zb time make
...
      ├─#946        00:00:31.058      0.360s     1.0%cpu (tree:    96.1%cpu)   37 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
      │ ├─#947      00:00:31.061      0.329s    96.3%cpu (tree:    96.3%cpu)   37 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
      │ └─#948      00:00:31.392      0.025s    97.9%cpu (tree:    97.9%cpu)    5 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
      ├─#949        00:00:31.424      0.047s     7.4%cpu (tree:    85.6%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/xxlimited_35.o -L/usr/l
      │ └─#950      00:00:31.428      0.042s     7.6%cpu (tree:    86.0%cpu)    9 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
      │   └─#951    00:00:31.431      0.038s    87.8%cpu (tree:    87.8%cpu)    9 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
      ├─#952        00:00:27.439      3.272s     0.2%cpu (tree:    97.5%cpu)   74 MB     0+5k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
      │ ├─#953      00:00:27.441      3.091s    97.6%cpu (tree:    97.6%cpu)   74 MB     0+4k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Include -I . -I /usr/incl
      │ └─#954      00:00:30.541      0.168s    97.9%cpu (tree:    97.9%cpu)   19 MB     0+1k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Include -I . -I /usr/include/x86_64-linux-gnu -I /usr/local/include -I
      ├─#955        00:00:30.721      0.114s     1.8%cpu (tree:    46.6%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/socketmodule.o -L/usr/l
      │ └─#956      00:00:30.727      0.108s     2.4%cpu (tree:    47.3%cpu)   10 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
      │   └─#957    00:00:30.732      0.101s    48.0%cpu (tree:    48.0%cpu)   10 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g
      ├─#958        00:00:30.848      0.422s     1.0%cpu (tree:    72.7%cpu)   39 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
      │ ├─#959      00:00:30.858      0.403s    73.6%cpu (tree:    73.6%cpu)   39 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Modules/_multiprocessing 
      │ └─#960      00:00:31.262      0.007s    87.0%cpu (tree:    87.0%cpu)    4 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Modules/_multiprocessing -I ./Include -I . -I /usr/include/x86_64-linu
      ├─#961        00:00:31.282      0.308s     1.1%cpu (tree:    97.5%cpu)   43 MB     0+0k iops    0 PF    3 threads   [rc=0] /usr/bin/gcc -fPIC -Wsign-compare -DNDEBUG -g -fwrapv -O3 -Wall -std=c11 -Wextra -Wno-unused-paramet
      │ ├─#962      00:00:31.285      0.291s    98.0%cpu (tree:    98.0%cpu)   43 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/cc1 -quiet -I ./Include/internal -I ./Modules/_multiprocessing 
      │ └─#963      00:00:31.577      0.013s    94.3%cpu (tree:    94.3%cpu)    5 MB     0+0k iops    0 PF    1 threads   [rc=0] as -I ./Include/internal -I ./Modules/_multiprocessing -I ./Include -I . -I /usr/include/x86_64-linu
      └─#964        00:00:31.595      0.056s     4.5%cpu (tree:    89.7%cpu)   16 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/bin/gcc -shared build/temp.linux-x86_64-3.11/home/aviad/cpython/Modules/_multiprocessing/multip
        └─#965      00:00:31.597      0.051s    15.2%cpu (tree:    92.8%cpu)    9 MB     0+0k iops    0 PF    2 threads   [rc=0] /usr/libexec/gcc/x86_64-linux-gnu/13/collect2 -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_pl
          └─#966    00:00:31.605      0.042s    95.0%cpu (tree:    95.0%cpu)    9 MB     0+0k iops    0 PF    1 threads   [rc=0] /usr/bin/ld -plugin /usr/libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so -plugin-opt=/usr/libexec/g

Group by command (most cpu-intensive last):
    0.000s    54.6%cpu (tree:    54.6%cpu)    1 MB avg    1 MB max  0+0k execs  cat
    0.001s    96.9%cpu (tree:    96.9%cpu)    2 MB avg    2 MB max  0+0k execs  /usr/bin/mkdir
    0.001s   105.6%cpu (tree:   105.6%cpu)    6 MB avg    6 MB max  0+0k execs  /bin/sh echo
    0.001s    56.9%cpu (tree:    56.9%cpu)    4 MB avg    4 MB max  0+0k execs  /bin/sh if
    0.001s    82.9%cpu (tree:    82.9%cpu)    2 MB avg    2 MB max  0+0k execs  sed
    0.001s    50.4%cpu (tree:    92.7%cpu)   20 MB avg   20 MB max  0+0k execs  sh gcc
    0.001s    40.1%cpu (tree:    71.7%cpu)    6 MB avg    6 MB max  0+0k execs  /bin/sh
    0.001s   101.1%cpu (tree:   101.1%cpu)    6 MB avg    6 MB max  0+0k execs  rm
    0.002s     0.0%cpu (tree:   759.0%cpu)    1 MB avg    1 MB max  0+0k execs  env make
    0.002s     0.0%cpu (tree:  1199.4%cpu)  117 MB avg  117 MB max 7+202k execs  /bin/sh case
    0.002s     1.4%cpu (tree:    96.0%cpu)   12 MB avg   17 MB max  1+1k execs  /bin/sh ./python
    0.003s     1.5%cpu (tree:    71.3%cpu)   51 MB avg   51 MB max 128+0k execs  /bin/sh gcc
    0.011s    96.8%cpu (tree:    96.8%cpu)    8 MB avg    8 MB max  0+0k execs  ./python import
    0.044s    95.7%cpu (tree:    95.7%cpu)    7 MB avg    8 MB max  0+1k execs  ./Programs/_freeze_module
    0.070s    56.2%cpu (tree:    56.2%cpu)   20 MB avg   51 MB max 128+0k execs  git
    0.113s    96.6%cpu (tree:    96.6%cpu)   17 MB avg   17 MB max  1+1k execs  ./python sysconfig
    0.180s    78.2%cpu (tree:    78.2%cpu)   55 MB avg   59 MB max 113+199k execs  ar
    0.226s     0.6%cpu (tree:   759.0%cpu)  313 MB avg  313 MB max 376+1074k execs  make
    0.304s     6.2%cpu (tree:    72.9%cpu)   13 MB avg   75 MB max 4+205k execs  /usr/libexec/gcc/x86_64-linux-gnu/13/collect2
    0.862s     0.5%cpu (tree:    99.1%cpu)   56 MB avg  313 MB max 94+652k execs  gcc
    1.337s    17.6%cpu (tree:  1199.9%cpu)  117 MB avg  117 MB max 7+202k execs  ./python ./setup.py
    3.266s    75.2%cpu (tree:    75.2%cpu)   13 MB avg   75 MB max 4+204k execs  /usr/bin/ld
    4.032s    98.8%cpu (tree:    98.8%cpu)   12 MB avg   83 MB max 25+18k execs  ./_bootstrap_python
    4.432s     4.5%cpu (tree:    91.3%cpu)   37 MB avg  117 MB max 3+197k execs  /usr/bin/gcc
   12.092s    91.6%cpu (tree:    91.6%cpu)   11 MB avg  148 MB max 4+133k execs  as
  236.988s    97.3%cpu (tree:    97.3%cpu)   56 MB avg  313 MB max 79+501k execs  /usr/libexec/gcc/x86_64-linux-gnu/13/cc1

env: 966 commands  34.778s   759.0%cpu    377+1074k iops   1364 PF  Exited 0
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
