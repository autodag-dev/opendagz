use std::collections::{hash_map, HashMap, HashSet};
use std::{fs, mem};
use std::path::PathBuf;
use chrono::{DateTime, Duration, Utc};
use nix::unistd::Pid;
use tracing::{debug, error, trace};

pub(crate) struct ProcessStart {
    ordinal: usize,
    pid: Pid,
    name: String,
    argv: Vec<String>,
    total_threads: usize,
    active_threads: HashSet<Pid>,
    start_time: DateTime<Utc>,
    cpu_time: Duration,
    elapsed: Duration,
}

#[derive(Debug, Clone)]
pub(crate) enum ProcessEnd {
    ExitCode(i32),
    Signal(i32),
    Exec
}

struct ProcessSpan {
    start: ProcessStart,
    end_time: DateTime<Utc>,
    rss_kb: i64,
    end: ProcessEnd,
}

impl ProcessSpan {
    pub(crate) fn elapsed(&self) -> Duration {
        self.end_time - self.start.start_time
    }
}

struct ThreadSpan {
    tid: Pid,
    pid: Option<Pid>,
    start_time: DateTime<Utc>,
    unbound_child_tids: Vec<Pid>,
    born_orphan: bool,

    proc_end: Option<ProcessEnd>,
}

impl ThreadSpan {
    fn new(tid: Pid, pid: Option<Pid>) -> Self {
        Self {
            tid,
            pid,
            start_time: Utc::now(),
            unbound_child_tids: Default::default(),
            born_orphan: pid.is_none(),
            proc_end: None,
        }
    }
    
    fn new_orphan(tid: Pid) -> Self {
        Self {
            tid,
            pid: None,
            start_time: Utc::now(),
            unbound_child_tids: Default::default(),
            born_orphan: true,
            proc_end: None,
        }
    }
    
    fn with_proc_end(mut self, proc_end: ProcessEnd) -> Self {
        self.proc_end = Some(proc_end);
        self
    }
}

#[derive(Default)]
pub(crate) struct ThreadMonitor {
    page_size_kb: i64,
    ticks_per_sec: f64,
    have_schedstats: bool,

    active_threads: HashMap<Pid, ThreadSpan>,
    pub(crate) active_procs: HashMap<Pid, ProcessStart>,
    finished_procs: Vec<ProcessSpan>,
}

impl ThreadMonitor {
    pub(crate) fn new() -> Self {
        let page_size_kb = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE).unwrap().unwrap() as i64 / 1024;
        let ticks_per_sec = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).unwrap().unwrap() as f64;
        let have_schedstats = fs::exists("/proc/self/schedstat").unwrap_or(false);
        Self {
            ticks_per_sec,
            page_size_kb,
            have_schedstats,
            ..Default::default()
        }
    }

    fn bind_tid(&mut self, tid: Pid, pid: Pid) {
        self.unbind_tid(tid, ProcessEnd::Exec);
        let thread = self.active_threads.get_mut(&tid).unwrap_or_else(|| {
            panic!("Failed to find thread with tid={} to bind to process", tid);
        });
        let proc = self.active_procs.get_mut(&pid).unwrap_or_else(|| {
            panic!("Failed to find process with pid={} to bind thread {}", pid, tid);
        });

        proc.total_threads += 1;
        if thread.proc_end.is_none() {
            proc.active_threads.insert(tid);
            thread.pid = Some(pid);
            trace!("Process #{} {} bound to thread {}, active={}",
                proc.ordinal, proc.pid, tid, proc.active_threads.len());
        } else {
            debug!("Thread {} is already finished, not binding to process #{} {}",
                tid, proc.ordinal, proc.pid);
        }
        
        if !thread.unbound_child_tids.is_empty() {
            let grandchildren = mem::take(&mut thread.unbound_child_tids);
            self.bind_tids(pid, grandchildren);
        }
    }

    fn unbind_tid(&mut self, tid: Pid, end: ProcessEnd) -> Option<Pid> {
        let thread = self.active_threads.get_mut(&tid)?;

        let proc = if let Some(pid) = thread.pid {
            self.active_procs.get_mut(&pid).unwrap_or_else(|| {
                panic!("Failed to find process with pid={} to unbind thread {}", pid, tid);
            })
        } else {
            return None;
        };

        let end_time = Utc::now();
        let cpu_time = if self.have_schedstats {
            let filename = format!("/proc/{}/task/{}/schedstat", tid, tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let parts: Vec<&str> = proc_stat.split_whitespace().collect();
                    Duration::nanoseconds(parts[0].parse::<i64>().unwrap_or(0))
                },
                Err(_) => Duration::zero(),
            }
        } else {
            match fs::read_to_string(format!("/proc/{}/stat", tid)) {
                Ok(proc_stat) => {
                    let post_parens = proc_stat.split_once(')').unwrap().1;
                    let parts: Vec<&str> = post_parens.split_whitespace().collect();
                    let user_time = parts[11].parse::<i64>().unwrap_or(0);
                    let sys_time = parts[12].parse::<i64>().unwrap_or(0);
                    Duration::milliseconds((1000.0 * (user_time + sys_time) as f64 / self.ticks_per_sec) as i64)
                },
                Err(_) => Duration::zero(),
            }
        };
        proc.elapsed += end_time - thread.start_time;
        proc.cpu_time += cpu_time;
        
        thread.start_time = end_time;
        thread.pid = None;
        proc.active_threads.remove(&tid);

        let proc_pid = proc.pid;
        if proc.active_threads.is_empty() {
            debug!("Process #{} {} pid={} finished by {:?}",
                proc.ordinal, proc.argv[0], proc.pid, end);
            let proc_stat = fs::read_to_string(format!("/proc/{}/stat", tid))
                .expect("Failed to read /proc/[pid]/stat");
            let post_parens = proc_stat.split_once(')').unwrap().1;
            let parts: Vec<&str> = post_parens.split_whitespace().collect();

            let rss_kb = parts[21].parse::<i64>().unwrap_or(0) * self.page_size_kb;
            let proc = self.active_procs.remove(&proc_pid).unwrap();
            let proc_span = ProcessSpan {
                start: proc,
                rss_kb,
                end_time,
                end,
            };
            self.finished_procs.push(proc_span);
        } else {
            trace!("Unbound thread {} from process #{} {} due to {:?}, active={}",
                tid, proc.ordinal, proc.pid, end, proc.active_threads.len());
    }
        
        Some(proc_pid)
    }
    
    fn bind_tids(&mut self, proc_pid: Pid, tids: Vec<Pid>, ) {
        for tid in tids {
            self.bind_tid(tid, proc_pid);
        }
    }

    pub(crate) fn start_thread(&mut self, tid: Pid, parent_tid: Pid, _event: i32) {
        let parent = self.active_threads.entry(parent_tid).or_insert_with(|| {
            debug!("Created orphan parent thread implicitly {} by tid={}", parent_tid, tid);
            ThreadSpan::new_orphan(parent_tid)
        });
        let pid = parent.pid;
        if pid.is_none() {
            parent.unbound_child_tids.push(tid);
        }
        let mut bind_proc = true;
        self.active_threads.entry(tid).and_modify(|thread| {
            if thread.born_orphan {
                let parent_proc = pid.and_then(|pid| self.active_procs.get(&pid));
                debug!("Updating thread {} born orphan, attaching parent_tid={} parent_proc=#{} {}",
                    tid, parent_tid, parent_proc.map(|p| p.ordinal).unwrap_or(0), pid.unwrap_or(Pid::from_raw(0)));
                // don't update the process - already created in start_proc
                bind_proc = false;
            }
        }).or_insert_with(|| ThreadSpan::new(tid, pid));

        if bind_proc {
            if let Some(pid) = pid {
                self.bind_tid(tid, pid);
            } else {
                debug!("Thread started {} without parent tid={}", tid, parent_tid);
            }
        }
    }

    pub(crate) fn start_proc(&mut self, pid: Pid, name: String, argv: Vec<String>, prev_tid: Option<Pid>) {
        let start_time = Utc::now();
        let mut born_orphan = false;
        let mut parent_pid = None;
        if let Some(prev_tid) = prev_tid {
            if let Some(parent_thread) = self.active_threads.get(&prev_tid) {
                debug!("Finishing previous process thread {} before new process {}", prev_tid, pid);
                parent_pid = parent_thread.pid;
                self.unbind_tid(prev_tid, ProcessEnd::Exec);
            } else {
                debug!("execing tid {} not found in active threads, born orphan; new pid={} exists={}",
                    prev_tid, pid, self.active_threads.contains_key(&pid));
                born_orphan = true;
            }
        }

        if self.active_procs.contains_key(&pid) {
            panic!("Process with pid={} already exists in active processes", pid);
        }

        let ordinal = self.active_procs.len() + self.finished_procs.len() + 1;
        let parent_proc = parent_pid.and_then(|pid| self.active_procs.get(&pid));
        debug!("Process #{} {} started with parent #{} with command: {}",
            ordinal, pid, parent_proc.map(|p| p.ordinal).unwrap_or(0), argv.join(" "));

        let proc_start = ProcessStart {
            ordinal,
            pid,
            name,
            argv,
            total_threads: 0,
            active_threads: Default::default(),
            start_time,
            cpu_time: Default::default(),
            elapsed: Default::default(),
        };
        self.active_procs.insert(pid, proc_start);

        self.active_threads.entry(pid).or_insert_with(|| ThreadSpan {
            tid: pid,
            pid: None,
            start_time,
            born_orphan,
            unbound_child_tids: Default::default(),
            proc_end: None,
        });

        self.bind_tid(pid, pid);
    }

    pub(crate) fn finish_thread(&mut self, tid: Pid, proc_end: ProcessEnd) {
        self.unbind_tid(tid, proc_end.clone());
        match self.active_threads.entry(tid) {
            hash_map::Entry::Occupied(mut entry) => {
                let thread = entry.get_mut();
                if thread.born_orphan {
                    debug!("Thread {tid} born orphan, left in active threads");
                    thread.proc_end = Some(proc_end);
                } else {
                    entry.remove();
                }
            },
            hash_map::Entry::Vacant(entry) => {
                error!("Finished thread {} died orphan, end={:?}", tid, proc_end);
                entry.insert(ThreadSpan::new_orphan(tid).with_proc_end(proc_end));
            }
        }
    }

    pub(crate) fn report(&self) {
        for proc in &self.active_procs {
            let proc = proc.1;
            println!("** Active process #{} {} pid={} has {} active threads: {}",
                proc.ordinal, proc.name, proc.pid, proc.active_threads.len(),
                     proc.active_threads.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
        }

        let mut total_cpu_time = Duration::zero();
        let total_elapsed = Utc::now() - self.finished_procs[0].start.start_time;
        for proc in &self.finished_procs {
            total_cpu_time += proc.start.cpu_time;
        }

        for proc in &self.finished_procs {
            let elapsed = proc.elapsed();
            let cpu_pct = 100.0 * proc.start.cpu_time.as_seconds_f64() / elapsed.as_seconds_f64();
            println!("#{:<4} {:20} {:?} {:3.1}%cpu {} threads  {}MB  {:.80}",
                proc.start.ordinal,
                PathBuf::from(&proc.start.argv[0]).file_name().unwrap().display(),
                elapsed.to_std().unwrap(),
                cpu_pct,
                proc.start.total_threads,
                proc.rss_kb / 1024,
                proc.start.argv.join(" ")
            );
        }

        println!("  {} processes, {:.1}%cpu",
            self.finished_procs.len(),
            100.0 * total_cpu_time.as_seconds_f64() / total_elapsed.as_seconds_f64());
    }
}
