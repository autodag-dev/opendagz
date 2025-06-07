use std::collections::{HashMap};
use std::cell::RefCell;
use std::fs;
use std::rc::Rc;
use std::time::{Instant};
use chrono::TimeDelta;
use nix::unistd::Pid;
use nix::libc;
use tracing::{debug, error, trace};

#[derive(Debug, Clone)]
pub(crate) enum ThreadEndReason {
    ExitCode(i32),
    LateExitCode(i32),
    Signal(nix::sys::signal::Signal),
    Exec
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ResourceUsage {
    pub max_rss_kb: i64,
    ucpu: TimeDelta,
    kcpu: TimeDelta,
    pub read_iops: i64,
    pub write_iops: i64,
    pub major_pf: i64,
    pub threads: i64,
}

impl ResourceUsage {
    pub(crate) fn cpu(&self) -> TimeDelta {
        self.ucpu + self.kcpu
    }

    pub(crate) fn sub(&mut self, other: &ResourceUsage) {
        self.ucpu -= other.ucpu;
        self.kcpu -= other.kcpu;
        self.read_iops -= other.read_iops;
        self.write_iops -= other.write_iops;
        self.major_pf -= other.major_pf;
        self.threads -= other.threads;
    }

    pub(crate) fn format_iops(&self) -> String {
        format!("{}+{}", self.read_iops, self.write_iops)
    }

    pub(crate) fn add_self_metrics(&mut self, other: &ResourceUsage) {
        // These metrics are collected per thread, rather than per tree.
        self.max_rss_kb = self.max_rss_kb.max(other.max_rss_kb);
        self.ucpu += other.ucpu;
        self.kcpu += other.kcpu;
        self.threads += other.threads;
    }

    pub(crate) fn add_all(&mut self, other: &ResourceUsage) {
        self.add_self_metrics(other);

        // These metrics are collected per tree.
        self.read_iops += other.read_iops;
        self.write_iops += other.write_iops;
        self.major_pf += other.major_pf;
    }
}


fn timeval_to_duration(tv: &libc::timeval) -> TimeDelta {
    TimeDelta::new(tv.tv_sec, (tv.tv_usec * 1000) as u32).unwrap()
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadEnd {
    pub(crate) usage: ResourceUsage,
    pub(crate) reason: ThreadEndReason,
}

impl ThreadEnd {
    pub(crate) fn from_rusage(reason: ThreadEndReason, rusage: &libc::rusage) -> Self {
        Self {
            usage: ResourceUsage {
                max_rss_kb: rusage.ru_maxrss,
                ucpu: timeval_to_duration(&rusage.ru_utime),
                kcpu: timeval_to_duration(&rusage.ru_stime),
                read_iops: rusage.ru_inblock,
                write_iops: rusage.ru_oublock,
                major_pf: rusage.ru_majflt,
                threads: 0,
            },
            reason,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ThreadInit {
    Unknown,
    Forked,
    Thread,
    Exec(Vec<String>)
}


pub(crate) struct ThreadSpan {
    pub(crate) tid: Pid,
    pub(crate) ordinal: usize,
    pub(crate) start_time: Instant,
    pub(crate) init: ThreadInit,

    /// The initial usage before end, or the final usage after end.
    pub(crate) usage: ResourceUsage,

    // Store refs rather than Pids, since execs recycle thread instances with the same tid.
    pub(crate) parent: Option<Rc<RefCell<ThreadSpan>>>,
    pub(crate) children: Vec<Rc<RefCell<ThreadSpan>>>,
    
    pub(crate) end_reason: Option<ThreadEndReason>,
    pub(crate) end_time: Instant,

    /// Aggregated usage from all child threads
    pub(crate) tree_usage: ResourceUsage,
}

impl ThreadSpan {
    fn new(tid: Pid, ordinal: usize) -> Self {
        let now = Instant::now();
        Self {
            tid,
            ordinal,
            init: ThreadInit::Unknown,
            parent: None,
            start_time: now,
            end_reason: None,
            children: Default::default(),
            tree_usage: Default::default(),
            usage: Default::default(),
            end_time: now,
        }
    }

    pub(crate) fn compile_tree(&mut self, nest_level: usize) {
        self.tree_usage = self.usage.clone();
        self.tree_usage.threads = 1;
        for child in &self.children {
            let mut child = child.borrow_mut();
            child.compile_tree(nest_level + 1);
            self.tree_usage.add_self_metrics(&child.tree_usage);
            match &child.init {
                ThreadInit::Exec(_) => {},
                ThreadInit::Forked | ThreadInit::Thread => {
                    // add usage only from sub-commands (recursively)
                    self.usage.add_self_metrics(&child.usage);
                }
                ThreadInit::Unknown => {
                    error!("Unknown thread: #{} {}", self.ordinal, self.tid);
                }
            }
            self.end_time = self.end_time.max(child.end_time);
        }
        let indent = "  ".repeat(nest_level);
        trace!("{:2} {indent}thread #{} {} cmd_io={} tree_io={}  kind={:?}",
            nest_level, self.ordinal, self.tid,
            self.usage.format_iops(),
            self.tree_usage.format_iops(),
            self.init,
        );
    }

    fn update_parent(&mut self, new_parent: Rc<RefCell<ThreadSpan>>) {
        let new_parent_ref = new_parent.borrow();
        let kind_was_exec = matches!(self.init, ThreadInit::Exec(_));
        match &mut self.parent {
            Some(parent) => {
                // Sometimes exec event is received *before* the spawn event.
                // In this case, the spawner's parent is actually the exec'ing thread's parent.
                assert!(kind_was_exec);
                debug!("binding grandparent of thread #{} {} to parent #{} {}",
                        self.ordinal, self.tid,
                        new_parent_ref.ordinal, new_parent_ref.tid);
                drop(new_parent_ref);
                parent.borrow_mut().update_parent(new_parent); 
            }
            None => {
                debug!("binding thread #{} {} to parent #{} {}",
                        self.ordinal, self.tid,
                        new_parent_ref.ordinal, new_parent_ref.tid);
                drop(new_parent_ref);
                self.parent = Some(new_parent);
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ThreadTracker {
    next_ordinal: usize,

    pub(crate) threads: HashMap<Pid, Rc<RefCell<ThreadSpan>>>,
    pub(crate) root: Option<Rc<RefCell<ThreadSpan>>>,
    pub have_schedstats: bool,
    ticks_per_sec: f64,
}

impl ThreadTracker {
    pub(crate) fn new() -> Self {
        let have_schedstats = fs::exists("/proc/self/schedstat").unwrap_or(false);
        let ticks_per_sec = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).unwrap().unwrap() as f64;
        Self {
            have_schedstats,
            ticks_per_sec,
            next_ordinal: 1,
            ..Default::default()
        }
    }

    fn get_thread(&mut self, tid: Pid) -> Rc<RefCell<ThreadSpan>> {
        self.threads.entry(tid).or_insert_with(|| {
            let ordinal = self.next_ordinal;
            self.next_ordinal += 1;
            Rc::new(RefCell::new(ThreadSpan::new(tid, ordinal)))
        }).clone()
    }

    fn read_cpu_usage(&self, tid: Pid, usage: &mut ResourceUsage, reason: ThreadEndReason) {
        usage.ucpu = if self.have_schedstats {
            let filename = format!("/proc/{}/task/{}/schedstat", tid, tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let parts: Vec<&str> = proc_stat.split_whitespace().collect();
                    TimeDelta::nanoseconds(parts[0].parse::<i64>().unwrap_or(0))
                },
                Err(e) => {
                    if !matches!(reason, ThreadEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    return;
                }
            }
        } else {
            let filename = format!("/proc/{}/task/{}/stat", tid, tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let post_parens = proc_stat.split_once(')').unwrap().1;
                    let parts: Vec<&str> = post_parens.split_whitespace().collect();
                    let user_time = parts[11].parse::<i64>().unwrap_or(0);
                    let sys_time = parts[12].parse::<i64>().unwrap_or(0);
                    TimeDelta::milliseconds((1000.0 * (user_time + sys_time) as f64 / self.ticks_per_sec) as i64)
                },
                Err(e) => {
                    if !matches!(reason, ThreadEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    return;
                },
            }
        };
        usage.kcpu = TimeDelta::zero();
    }

    pub(crate) fn handle_spawn(&mut self, tid: Pid, parent_tid: Pid, is_fork: bool) {
        let parent = self.get_thread(parent_tid);
        let thread = self.get_thread(tid);
        {
            let mut thread = thread.borrow_mut();
            thread.update_parent(parent.clone());

            thread.init = if is_fork { ThreadInit::Forked } else { ThreadInit::Thread };
            trace!("spawned thread #{} {} with parent #{} {} forked={is_fork}",
                thread.ordinal, thread.tid, parent.borrow().ordinal, parent.borrow().tid);
        }
        parent.borrow_mut().children.push(thread.clone());
    }

    pub(crate) fn handle_exec(&mut self, pid: Pid, argv: Vec<String>, prev_tid: Option<Pid>, end: ThreadEnd) {
        let parent = if let Some(prev_tid) = prev_tid {
            // finish and store previous thread
            let prev_exists = self.threads.contains_key(&prev_tid);
            let prev_thread = self.finish_thread(prev_tid, end);
            if !prev_exists {
                let prev_thread = prev_thread.borrow();
                debug!("Exec with unknown prev_tid: #{} {}", prev_thread.ordinal, prev_thread.tid);
            }
            self.threads.remove(&prev_tid);
            Some(prev_thread)
        } else {
            None
        };

        let new_thread = self.get_thread(pid);
        {
            let mut new_thread = new_thread.borrow_mut();
            new_thread.init = ThreadInit::Exec(argv.clone());
            new_thread.parent = parent.clone();
            if let Some(parent) = &parent {
                // Set initial usage to parent's exit usage
                new_thread.usage = parent.borrow().usage.clone();
            }
            
            debug!("new command: tid=#{} {} parent=#{} {} {}",
                new_thread.ordinal, pid,
                if let Some(parent) = &parent { parent.borrow().ordinal } else {0},
                if let Some(parent) = &parent { parent.borrow().tid } else {Pid::from_raw(0)},
                argv.join(" "));
        }
        if let Some(parent) = &parent {
            parent.borrow_mut().children.push(new_thread.clone());
        }

        if prev_tid.is_none() {
            assert!(self.root.is_none(), "Root process already exists, cannot exec without previous thread");
            self.root = Some(new_thread.clone())
        }
    }

    pub(crate) fn finish_thread(&mut self, tid: Pid, mut proc_end: ThreadEnd) -> Rc<RefCell<ThreadSpan>> {
        let thread = self.get_thread(tid);
        {
            let mut thread = thread.borrow_mut();
            thread.end_time = Instant::now();
            if thread.end_reason.is_none() {
                self.read_cpu_usage(thread.tid, &mut proc_end.usage, proc_end.reason.clone());
                let initial_usage = thread.usage.clone();
                trace!("thread end #{} {} cpu={:.0}ms initial={:.0}ms elapsed={:.0}ms",
                    thread.ordinal, thread.tid,
                    proc_end.usage.cpu().as_seconds_f64() * 1000.0,
                    initial_usage.cpu().as_seconds_f64() * 1000.0,
                    (thread.end_time - thread.start_time).as_secs_f64() * 1000.0,
                );
                thread.usage = proc_end.usage;
                thread.usage.sub(&initial_usage);

                // fix end cpu - read thread-specific stats
                thread.end_reason = Some(proc_end.reason);
            } else if matches!(proc_end.reason, ThreadEndReason::Signal(_)) && !matches!(thread.end_reason, Some(ThreadEndReason::Signal(_))) {
                // Signal end reason is "stronger" than other reasons
                thread.end_reason = Some(proc_end.reason);
            }
        }
        thread
    }
}
