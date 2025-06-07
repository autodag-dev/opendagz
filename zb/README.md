# OpenDAGz

Dagz.run is building an Accelerated CI Platform.
OpenDagz is the community project by Dagz, providing several tools and libraries
for the benefit of the developers community.

Currently OpenDagz provides:

## `zb time`
A time(1) substitude that prints the entire command tree and relevant performance metrics.

It is useful to analyze the behavior of complex build systems.

Current supported on Linux, with MacOS support underway.

### Requirements
* `zb time` uses ptrace(2) to track child processes and resources. This should work most of the time.
  Consult [ptrace(2) man page](https://man7.org/linux/man-pages/man2/ptrace.2.html) if you're hitting permission errors. 
* For precise CPU measurements, a modern Linux kernel with [schedstat](https://docs.kernel.org/scheduler/sched-stats.html) enabled (). This should work most of the time.


# Installation

* You can download the latest releases from:
  https://github.com/autodag-dev/opendagz/releases

* Alternatively, install using cargo::

  `cargo install dagz_zb`
