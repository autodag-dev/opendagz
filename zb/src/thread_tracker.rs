use std::collections::{HashMap};
use std::cell::RefCell;
use std::fs;
use std::rc::Rc;
use std::time::{Instant};
use chrono::TimeDelta;
use nix::unistd::Pid;
use nix::libc;
use tracing::{debug, error, trace};
use crate::DagzCommands::Time;

#[derive(Debug, Clone)]
pub(crate) enum ProcessEndReason {
    ExitCode(i32),
    LateExitCode(i32),
    Signal(i32),
    Exec
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ResourceUsage {
    pub(crate) max_rss_kb: i64,
    pub(crate) ucpu: chrono::TimeDelta,
    kcpu: chrono::TimeDelta,
    iops: i64,
    pub(crate) threads: i64,
}

impl ResourceUsage {
    pub(crate) fn set_max(&mut self, other: &ResourceUsage) {
        self.ucpu = self.ucpu.max(other.ucpu);
        self.kcpu = self.kcpu.max(other.kcpu);
        self.iops = self.iops.max(other.iops);
    }
}

impl ResourceUsage {
    pub(crate) fn cpu(&self) -> chrono::TimeDelta {
        self.ucpu + self.kcpu
    }

    pub(crate) fn sub(&mut self, other: &ResourceUsage) {
        self.ucpu -= other.ucpu;
        self.kcpu -= other.kcpu;
        self.iops -= other.iops
    }

    pub(crate) fn add(&mut self, other: &ResourceUsage) {
        self.max_rss_kb = self.max_rss_kb.max(other.max_rss_kb);
        self.ucpu += other.ucpu;
        self.kcpu += other.kcpu;
        self.iops += other.iops;
    }
}


fn timeval_to_duration(tv: &libc::timeval) -> chrono::TimeDelta {
    chrono::TimeDelta::new(tv.tv_sec, (tv.tv_usec * 1000) as u32).unwrap()
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadEnd {
    time: Instant,
    pub(crate) usage: ResourceUsage,
    pub(crate) reason: ProcessEndReason,
}

impl ThreadEnd {
    pub(crate) fn from_rusage(reason: ProcessEndReason, rusage: &libc::rusage) -> Self {
        Self {
            time: Instant::now(),
            usage: ResourceUsage {
                max_rss_kb: rusage.ru_maxrss,
                ucpu: timeval_to_duration(&rusage.ru_utime),
                kcpu: timeval_to_duration(&rusage.ru_stime),
                iops: rusage.ru_inblock + rusage.ru_oublock,
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

    /// Used to deduct initial counters not reset after execve()
    initial_usage: Option<ResourceUsage>,

    // Store refs rather than Pids, since execs recycle thread instances with the same tid.
    pub(crate) parent: Option<Rc<RefCell<ThreadSpan>>>,
    pub(crate) children: Vec<Rc<RefCell<ThreadSpan>>>,
    
    pub(crate) end: Option<ThreadEnd>,

    pub(crate) tree_usage: ResourceUsage,
    pub(crate) children_usage: ResourceUsage,
    pub(crate) tree_end_time: Instant,
}

impl ThreadSpan {
    fn new(tid: Pid, ordinal: usize) -> Self {
        let now = Instant::now();
        Self {
            tid,
            ordinal,
            init: ThreadInit::Unknown,
            parent: None,
            initial_usage: None,
            start_time: now,
            end: None,
            children: Default::default(),
            tree_usage: Default::default(),
            children_usage: Default::default(),
            tree_end_time: now,
        }
    }

    pub(crate) fn compile_tree(&mut self, nest_level: usize) {
        let end = self.end.as_mut().unwrap();
        self.tree_usage = end.usage.clone();
        if let Some(initial) = &self.initial_usage {
            self.tree_usage.sub(initial);
        }

        self.tree_end_time = end.time;
        self.tree_usage.threads = 1;
        for child in &self.children {
            let mut child = child.borrow_mut();
            child.compile_tree(nest_level + 1);
            match &child.init {
                ThreadInit::Exec(_) => {
                    // add usage from this sub-commands
                    self.children_usage.add(&child.tree_usage);
                },
                ThreadInit::Forked | ThreadInit::Thread => {
                    // add usage only from sub-commands (recursively)
                    self.children_usage.add(&child.children_usage);
                    self.tree_usage.threads += child.tree_usage.threads;

                    self.tree_end_time = self.tree_end_time.max(child.tree_end_time);
                }
                ThreadInit::Unknown => {
                    error!("Unknown thread: #{} {}", self.ordinal, self.tid);
                }
            }

            // sometimes threads are not waited for, and thus the main thread's rusage is not accumulated.
            // we ensure that the parent's usage is always at least as the last child's usage.
            self.tree_usage.set_max(&child.tree_usage);
        }
        let indent = "  ".repeat(nest_level);
        trace!("{:2} {indent}thread #{} {} initial={:.0} end={:.0} tree_cpu={:.0} child_cpu={:.0}  kind={:?}",
            nest_level, self.ordinal, self.tid,
            if let Some(initial) = &self.initial_usage { initial.cpu().as_seconds_f64()*1000.0 } else { 0.0 },
            if let Some(end) = &self.end { end.usage.cpu().as_seconds_f64()*1000.0 } else { 0.0 },
            self.tree_usage.cpu().as_seconds_f64() * 1000.0,
            self.children_usage.cpu().as_seconds_f64() * 1000.0,
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

    execd_threads: Vec<Rc<RefCell<ThreadSpan>>>,
    pub(crate) threads: HashMap<Pid, Rc<RefCell<ThreadSpan>>>,
    pub(crate) root: Option<Rc<RefCell<ThreadSpan>>>,
    have_schedstats: bool,
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

    fn get_cpu_usage(&self, thread: &ThreadSpan, reason: ProcessEndReason) -> TimeDelta {
        if self.have_schedstats {
            let filename = format!("/proc/{}/task/{}/schedstat", thread.tid, thread.tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let parts: Vec<&str> = proc_stat.split_whitespace().collect();
                    TimeDelta::nanoseconds(parts[0].parse::<i64>().unwrap_or(0))
                },
                Err(e) => {
                    if !matches!(reason, ProcessEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    TimeDelta::zero()
                }
            }
        } else {
            let filename = format!("/proc/{}/stat", thread.tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let post_parens = proc_stat.split_once(')').unwrap().1;
                    let parts: Vec<&str> = post_parens.split_whitespace().collect();
                    let user_time = parts[11].parse::<i64>().unwrap_or(0);
                    let sys_time = parts[12].parse::<i64>().unwrap_or(0);
                    TimeDelta::milliseconds((1000.0 * (user_time + sys_time) as f64 / self.ticks_per_sec) as i64)
                },
                Err(e) => {
                    if !matches!(reason, ProcessEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    TimeDelta::zero()
                },
            }
        }
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
            let prev_thread = self.get_thread(prev_tid);
            {
                let mut prev_thread = prev_thread.borrow_mut();
                prev_thread.end = Some(end.clone());
                if !prev_exists {
                    debug!("Exec with unknown prev_tid: #{} {}", prev_thread.ordinal, prev_thread.tid)
                }
            }

            self.execd_threads.push(prev_thread.clone());
            self.threads.remove(&prev_tid);
            Some(prev_thread)
        } else {
            None
        };

        let new_thread = self.get_thread(pid);
        {
            let mut new_thread = new_thread.borrow_mut();
            new_thread.init = ThreadInit::Exec(argv.clone());
            new_thread.initial_usage = Some(end.usage);
            new_thread.parent = parent.clone();

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

    pub(crate) fn finish_thread(&mut self, tid: Pid, proc_end: ThreadEnd) {
        let thread = self.get_thread(tid);
        let mut thread = thread.borrow_mut();
        trace!("thread end #{} {} cpu={:.1}ms", thread.ordinal, thread.tid, proc_end.usage.cpu().as_seconds_f64() * 1000.0);
        thread.end = Some(proc_end);
    }
}

