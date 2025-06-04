use std::collections::{hash_map, HashMap, HashSet};
use std::{io, mem};
use std::cell::RefCell;
use std::io::stdout;
use std::os::fd::{AsFd};
use std::rc::Rc;
use std::time::{Duration, Instant};
use nix::unistd::Pid;
use tracing::{debug, error, trace};
use colored::Colorize;
use nix::libc;

#[derive(Debug, Clone)]
pub(crate) enum ProcessEndReason {
    ExitCode(i32),
    LateExitCode(i32),
    Signal(i32),
    Exec
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessEnd {
    max_rss_kb: i64,
    iops: i64,
    ucpu: Duration,
    kcpu: Duration,
    
    reason: ProcessEndReason,
}

fn timeval_to_duration(tv: &libc::timeval) -> Duration {
    Duration::new(tv.tv_sec as u64, (tv.tv_usec * 1000) as u32)
}

impl ProcessEnd {
    pub(crate) fn from_rusage(reason: ProcessEndReason, rusage: &libc::rusage) -> Self {
        Self {
            max_rss_kb: rusage.ru_maxrss,
            ucpu: timeval_to_duration(&rusage.ru_utime),
            kcpu: timeval_to_duration(&rusage.ru_stime),
            iops: rusage.ru_inblock + rusage.ru_oublock,
            reason,
        }
    }
}

pub(crate) struct ProcessSpan {
    // Init args
    ordinal: usize,
    pid: Pid,
    argv: Vec<String>,
    start_time: Instant,
    
    /// Used to deduct initial counters not reset after execve()
    initial_usage: ProcessEnd,

    // Relations
    parent: Option<Rc<RefCell<ProcessSpan>>>,
    active_threads: HashSet<Pid>,
    children: Vec<Rc<RefCell<ProcessSpan>>>,

    // Accumulators
    total_threads: usize,
    
    /// Tree-cpu is what we get from wait4().  
    tree_cpu_time: Duration,
    
    /// Calculated by subtracting all children tree_cpu_time from self.tree_cpu_time.
    self_cpu_time: Duration,
    
    /// Total durations of all threads and forked processes (until execve() calls).
    self_elapsed: Duration,

    /// Time of last ended thread.
    tree_end_time: Instant,
    end: Option<ProcessEnd>,
}

impl ProcessSpan {
    pub(crate) fn elapsed(&self) -> Duration {
        self.tree_end_time - self.start_time
    }

    fn compile_tree(&mut self) -> usize {
        let mut depth = 0;
        self.tree_cpu_time -= self.initial_usage.ucpu + self.initial_usage.kcpu;
        self.self_cpu_time = self.tree_cpu_time; // reset self_cpu_time to tree_cpu_time for the root process
        for child in &self.children {
            let mut child = child.borrow_mut();
            let child_depth = child.compile_tree();
            depth = depth.max(1 + child_depth);
            self.tree_end_time = self.tree_end_time.max(child.tree_end_time);
            self.self_cpu_time = self.self_cpu_time.saturating_sub(child.tree_cpu_time);
        }
        depth
    }
}

struct ThreadSpan {
    proc: Option<Rc<RefCell<ProcessSpan>>>,
    start_time: Instant,
    unbound_child_tids: Vec<Pid>,

    /// for execing threads, the associated proces if no ppid is known during exec
    orphan_proc: Option<Rc<RefCell<ProcessSpan>>>,

    /// Threads born without parent will be kept in active threads even after they have finished.
    born_orphan: bool,

    end: Option<ProcessEnd>,
}

impl ThreadSpan {
    fn new_orphan() -> Self {
        Self {
            proc: None,
            start_time: Instant::now(),
            unbound_child_tids: Default::default(),
            orphan_proc: None,
            born_orphan: true,
            end: None,
        }
    }

    fn with_end(mut self, proc_end: ProcessEnd) -> Self {
        self.end = Some(proc_end);
        self
    }
}

#[derive(Default)]
struct ProcessGroup {
    num_execs: usize,
    total_rss_kb: i64,
    max_rss_kb: i64,
    total_elapsed: Duration,
    total_self_cpu_time: Duration,
    total_tree_cpu_time: Duration,
}

impl ProcessGroup {
    fn add(&mut self, proc: Rc<RefCell<ProcessSpan>>) {
        let proc = proc.borrow();
        let rss_kb = if let Some(end) = proc.end.as_ref() { end.max_rss_kb } else { 0 };
        self.num_execs += 1;
        self.total_rss_kb += rss_kb;
        self.max_rss_kb = self.max_rss_kb.max(rss_kb);
        self.total_elapsed += proc.elapsed();
        self.total_self_cpu_time += proc.self_cpu_time;
        self.total_tree_cpu_time += proc.tree_cpu_time;
    }
}


#[derive(Default)]
pub(crate) struct ThreadMonitor {
    ticks_per_sec: f64,
    have_schedstats: bool,
    is_tty: bool,

    active_threads: HashMap<Pid, ThreadSpan>,
    pub(crate) active_procs: HashMap<Pid, Rc<RefCell<ProcessSpan>>>,
    finished_procs: Vec<Rc<RefCell<ProcessSpan>>>,
    root: Option<Rc<RefCell<ProcessSpan>>>,
}

impl ThreadMonitor {
    pub(crate) fn new() -> Self {
        //let page_size_kb = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE).unwrap().unwrap() as i64 / 1024;
        let ticks_per_sec = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).unwrap().unwrap() as f64;
        //let have_schedstats = fs::exists("/proc/self/schedstat").unwrap_or(false);
        let is_tty = nix::unistd::isatty(stdout().as_fd()).unwrap_or(false);
        Self {
            ticks_per_sec,
            //page_size_kb,
            //have_schedstats,
            is_tty,
            ..Default::default()
        }
    }

    fn bind_tid(&mut self, tid: Pid, pid: Pid) {
        let thread = match self.active_threads.get_mut(&tid) {
            Some(thread) => thread,
            None => {
                error!("Failed to find thread with tid={} to bind to process", tid);
                return;
            }
        };

        let proc_rc = match self.active_procs.get(&pid) {
            Some(proc) => proc.clone(),
            None => {
                error!("Failed to find process with pid={} to bind thread {}", pid, tid);
                return;
            }
        };

        if tid != pid {
            if let Some(orphan_proc_rc) = thread.orphan_proc.take() {
                let mut unbound_proc = orphan_proc_rc.borrow_mut();
                debug!("try bind pid {} to ppid {pid}", unbound_proc.pid);
                unbound_proc.parent = Some(proc_rc.clone());
                proc_rc.borrow_mut().children.push(orphan_proc_rc.clone());
            }
        }

        if let Some(cur_proc_rc) = &thread.proc {
            debug!("Thread {} is already bound to process {}, not rebinding, unbound_proc={}",
                tid, cur_proc_rc.borrow().pid, thread.orphan_proc.is_some());

            return;
        }

        let mut proc = proc_rc.borrow_mut();
        proc.total_threads += 1;
        if thread.end.is_none() {
            proc.active_threads.insert(tid);
            thread.proc = Some(proc_rc.clone());
            trace!("Process #{} {} bound to thread {}, active={}",
                proc.ordinal, proc.pid, tid, proc.active_threads.len());
        } else {
            debug!("Thread {} is already finished, not binding to process #{} {}",
                tid, proc.ordinal, proc.pid);
        }
        drop(proc);

        if !thread.unbound_child_tids.is_empty() {
            let grandchildren = mem::take(&mut thread.unbound_child_tids);
            for tid in grandchildren {
                self.bind_tid(tid, pid);
            }
        }
    }

    fn unbind_tid(&mut self, tid: Pid, end: ProcessEnd) -> Option<Rc<RefCell<ProcessSpan>>> {
        let thread = match self.active_threads.get_mut(&tid) {
            Some(thread) => thread,
            None => {
                debug!("Thread {} not found in active threads, cannot unbind", tid);
                return None;
            }
        };

        let proc_rc = if let Some(proc) = &thread.proc {
            proc
        } else {
            debug!("Thread {} process not found in active processes, cannot unbind", tid);
            return None;
        };

        let end_time = Instant::now();
        let mut proc = proc_rc.borrow_mut();
        proc.self_elapsed += end_time - thread.start_time;
        proc.tree_cpu_time = end.ucpu + end.kcpu;
        proc.tree_end_time = proc.tree_end_time.max(end_time);

        thread.start_time = end_time;
        proc.active_threads.remove(&tid);

        let proc_pid = proc.pid;
        if proc.active_threads.is_empty() {
            debug!("Process #{} {} pid={} finished by {:?}",
                proc.ordinal, proc.argv[0], proc.pid, end.reason);

            self.active_procs.remove(&proc_pid).unwrap();
            proc.end = Some(end);
            self.finished_procs.push(proc_rc.clone());
        } else {
            trace!("Unbound thread {} from process #{} {} due to {:?}, active={}",
                tid, proc.ordinal, proc.pid, end.reason, proc.active_threads.len());
        }

        drop(proc);
        let cloned_proc_rc = proc_rc.clone();
        thread.proc = None;
        Some(cloned_proc_rc)
    }

    pub(crate) fn start_thread(&mut self, tid: Pid, parent_tid: Pid, _event: i32) {
        let parent = self.active_threads.entry(parent_tid).or_insert_with(|| {
            debug!("Created orphan parent thread implicitly {} by tid={}", parent_tid, tid);
            ThreadSpan::new_orphan()
        });
        let proc = parent.proc.as_ref().cloned();
        if proc.is_none() {
            debug!("Thread started {} without parent tid={}", tid, parent_tid);
            parent.unbound_child_tids.push(tid);
        }
        self.active_threads.entry(tid).or_insert_with(ThreadSpan::new_orphan);

        if let Some(proc) = &proc {
            let pid = proc.borrow().pid;
            self.bind_tid(tid, pid);
        }
    }

    pub(crate) fn handle_exec(&mut self, pid: Pid, argv: Vec<String>, prev_tid: Option<Pid>, end: ProcessEnd) {
        let start_time = Instant::now();
        let mut born_orphan = false;
        let mut parent_proc = None;
        if let Some(prev_tid) = prev_tid {
            if let Some(_parent_thread) = self.active_threads.get(&prev_tid) {
                trace!("Finishing previous process thread {} before new process {}", prev_tid, pid);
                parent_proc = self.unbind_tid(prev_tid, end.clone());
            } else {
                debug!("execing tid {} not found in active threads, born orphan; new pid={} exists={}",
                    prev_tid, pid, self.active_threads.contains_key(&pid));
                born_orphan = true;
            }
        }

        let ordinal = self.active_procs.len() + self.finished_procs.len() + 1;
        debug!("Process #{} {} started with parent #{} with command: {}",
            ordinal, pid, parent_proc.clone().map(|p| p.borrow().ordinal).unwrap_or(0), argv.join(" "));

        let new_proc = Rc::new(RefCell::new(ProcessSpan {
            ordinal,
            parent: parent_proc.clone(),
            pid,
            argv,
            total_threads: 0,
            active_threads: Default::default(),
            start_time,
            initial_usage: end,
            self_cpu_time: Default::default(),
            tree_cpu_time: Default::default(),
            self_elapsed: Default::default(),
            children: Default::default(),
            tree_end_time: start_time,
            end: None,
        }));
        if ordinal == 1 {
            self.root = Some(new_proc.clone());
        }
        if let Some(parent_proc) = &parent_proc {
            parent_proc.borrow_mut().children.push(new_proc.clone());
        }

        self.active_threads.entry(pid).or_insert_with(|| ThreadSpan {
            //tid: pid,
            proc: None,
            start_time,
            born_orphan,
            unbound_child_tids: Default::default(),
            end: None,
            orphan_proc: if born_orphan { Some(new_proc.clone()) } else { None },
        });
        self.active_procs.insert(pid, new_proc);

        self.bind_tid(pid, pid);
    }

    pub(crate) fn finish_thread(&mut self, tid: Pid, proc_end: ProcessEnd) -> Option<Rc<RefCell<ProcessSpan>>> {
        let finished_proc = self.unbind_tid(tid, proc_end.clone());
        if let Some(finished_proc) = &finished_proc {
            if matches!(proc_end.reason, ProcessEndReason::LateExitCode(_)) {
                debug!("finished thread without exit event: {} pid={}", tid, finished_proc.borrow().pid);
            }
        }
        match self.active_threads.entry(tid) {
            hash_map::Entry::Occupied(mut entry) => {
                let thread = entry.get_mut();
                if thread.born_orphan {
                    debug!("Thread {tid} born orphan, left in active threads");
                    thread.end = Some(proc_end);
                } else {
                    entry.remove();
                }
            },
            hash_map::Entry::Vacant(entry) => {
                debug!("Finished thread {} died orphan, end={:?}", tid, proc_end);
                entry.insert(ThreadSpan::new_orphan().with_end(proc_end));
            }
        }

        finished_proc
    }

    fn print_tree(&self, output: &mut dyn io::Write, proc: &ProcessSpan, indent: &mut String, postfix: &mut String, last: bool, is_root: bool) {
        let proc_end = proc.end.as_ref().unwrap();
        let elapsed = proc.elapsed();
        let connector = if is_root { "" } else if last { "└─" } else { "├─" };

        let argv_cutoff = if self.is_tty { 100 } else { 0 };
        writeln!(output, "{indent}{connector}{:<5}{postfix} {:9.3}s {:7.1}%cpu {:4} MB (tree: {:7.1}%cpu) {:>4} threads {:>8} {:.argv_cutoff$}",
             format!("#{}", proc.ordinal),
             elapsed.as_secs_f64(),
             100.0 * proc.self_cpu_time.as_secs_f64() / elapsed.as_secs_f64(),
             proc_end.max_rss_kb / 1024,
             100.0 * proc.tree_cpu_time.as_secs_f64() / elapsed.as_secs_f64(),
             proc.total_threads,
             match proc_end.reason {
                 ProcessEndReason::ExitCode(code) | ProcessEndReason::LateExitCode(code) => {
                     if code == 0 { format!("[rc={}]", code).normal() } else { format!("[rc={}]", code).bright_red() }
                 },
                 ProcessEndReason::Signal(signal) => format!("[killed by {}]", signal).bright_red(),
                 ProcessEndReason::Exec => "[exec]".to_string().normal(),
             },
             proc.argv.join(" "),
        ).expect("Failed to write to output");

        let prev_indent_len = indent.len();

        postfix.truncate(postfix.len() - 2);
        if !is_root {
            indent.push_str(if last { "  " } else { "│ " });
        }
        for (num, child) in proc.children.iter().enumerate() {
            self.print_tree(output, &child.borrow(), indent, postfix, num == proc.children.len() - 1, false);
        }
        postfix.push_str("  ");
        if !is_root {
            indent.truncate(prev_indent_len);
        }
    }

    pub(crate) fn report(&self, output: &mut dyn io::Write) {
        let mut proc_groups: HashMap<String, ProcessGroup> = Default::default();
        let mut root = self.root.as_ref().unwrap().borrow_mut();
        let tree_depth = root.compile_tree();
        drop(root);
        let root = self.root.as_ref().unwrap().borrow();

        for proc_rc in self.active_procs.values() {
            let proc = proc_rc.borrow();
            proc_groups.entry(proc.argv[0].clone()).or_default().add(proc_rc.clone());
            error!("Active process #{} {} pid={} has {} active threads: {}",
                proc.ordinal, proc.argv[0], proc.pid, proc.active_threads.len(),
                     proc.active_threads.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
        }

        let mut total_cpu_time = Duration::default();
        let total_elapsed = Instant::now() - root.start_time;

        let mut orphans = Vec::new();
        for proc_rc in &self.finished_procs {
            let proc = proc_rc.borrow();
            proc_groups.entry(proc.argv[0].clone()).or_default().add(proc_rc.clone());
            total_cpu_time += proc.self_cpu_time;
            if proc.ordinal != 1 && proc.parent.is_none() {
                error!("process without parent: #{} {}", proc.ordinal, proc.pid);
                orphans.push(proc_rc.clone());
            }
        }
        let mut postfix = "  ".repeat(tree_depth + 1);

        self.print_tree(output, &root, &mut String::new(), &mut postfix, true, true);
        for (num, orphan) in orphans.iter().enumerate() {
            let orphan = orphan.borrow();
            self.print_tree(output, &orphan, &mut String::from("  "), &mut postfix, num == orphans.len() - 1, false);
        }

        if proc_groups.values().any(|group| group.num_execs >= 3) {
            writeln!(output, "\nGroup by command:").expect("Failed to write to output");
            let mut proc_groups = proc_groups.iter().collect::<Vec<_>>();
            proc_groups.sort_by_key(|(_, group)| group.total_self_cpu_time);
            for (name, group) in proc_groups {
                writeln!(output, "{:>9.3}s {:>7.1}%cpu {:4} MB avg {:4} MB max (tree: {:7.1}%cpu) {:>5} execs  {name}",
                    group.total_self_cpu_time.as_secs_f64(),
                    100.0 * group.total_self_cpu_time.as_secs_f64() / group.total_elapsed.as_secs_f64(),
                    group.total_rss_kb / 1024 / group.num_execs as i64,
                    group.max_rss_kb / 1024,
                    100.0 * group.total_tree_cpu_time.as_secs_f64() / group.total_elapsed.as_secs_f64(),
                    group.num_execs,
                ).expect("Failed to write to output");
            }
        }

        writeln!(output, "{}: {} processes {:7.3}s {:7.1}%cpu",
            root.argv[0],
            self.finished_procs.len(),
            total_elapsed.as_secs_f64(),
            100.0 * total_cpu_time.as_secs_f64() / total_elapsed.as_secs_f64()
        ).expect("Failed to write to output");
    }
}
