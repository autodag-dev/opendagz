use std::collections::{hash_map, HashMap, HashSet};
use std::{fs, mem, ptr};
use std::cell::RefCell;
use std::io::stdout;
use std::os::fd::{AsFd, AsRawFd};
use std::rc::Rc;
use chrono::{DateTime, Duration, Utc};
use nix::unistd::Pid;
use tracing::{debug, error, trace};
use colored::Colorize;

struct ProcessEnd {
    end_time: DateTime<Utc>,
    rss_kb: i64,
    reason: ProcessEndReason,
}

pub(crate) struct ProcessSpan {
    ordinal: usize,
    parent: Option<Rc<RefCell<ProcessSpan>>>,
    pid: Pid,
    argv: Vec<String>,
    total_threads: usize,
    active_threads: HashSet<Pid>,
    start_time: DateTime<Utc>,
    cpu_time: Duration,
    elapsed: Duration,
    children: Vec<Rc<RefCell<ProcessSpan>>>,
    end: Option<ProcessEnd>,
}

#[derive(Debug, Clone)]
pub(crate) enum ProcessEndReason {
    ExitCode(i32),
    LateExitCode(i32),
    Signal(i32),
    Exec
}

impl ProcessSpan {
    pub(crate) fn elapsed(&self) -> Duration {
        self.end.as_ref().unwrap().end_time - self.start_time
    }

    fn tree_depth(&self) -> usize {
        let mut depth = 0;
        for child in &self.children {
            depth = depth.max(1 + child.borrow().tree_depth());
        }
        depth
    }
}

struct ThreadSpan {
    tid: Pid,
    proc: Option<Rc<RefCell<ProcessSpan>>>,
    start_time: DateTime<Utc>,
    last_cpu_time: Duration,
    unbound_child_tids: Vec<Pid>,
    orphan_proc: Option<Rc<RefCell<ProcessSpan>>>,  // for execing threads, the associated proces if no ppid is known during exec
    born_orphan: bool,

    end_reason: Option<ProcessEndReason>,
}

impl ThreadSpan {
    fn new_orphan(tid: Pid) -> Self {
        Self {
            tid,
            proc: None,
            start_time: Utc::now(),
            last_cpu_time: Default::default(),
            unbound_child_tids: Default::default(),
            orphan_proc: None,
            born_orphan: true,
            end_reason: None,
        }
    }

    fn with_end_reason(mut self, proc_end: ProcessEndReason) -> Self {
        self.end_reason = Some(proc_end);
        self
    }
}

#[derive(Default)]
pub(crate) struct ThreadMonitor {
    page_size_kb: i64,
    ticks_per_sec: f64,
    have_schedstats: bool,
    is_tty: bool,

    active_threads: HashMap<Pid, ThreadSpan>,
    pub(crate) active_procs: HashMap<Pid, Rc<RefCell<ProcessSpan>>>,
    finished_procs: Vec<Rc<RefCell<ProcessSpan>>>,
}

impl ThreadMonitor {
    pub(crate) fn new() -> Self {
        let page_size_kb = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE).unwrap().unwrap() as i64 / 1024;
        let ticks_per_sec = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).unwrap().unwrap() as f64;
        let have_schedstats = fs::exists("/proc/self/schedstat").unwrap_or(false);
        let is_tty = nix::unistd::isatty(stdout().as_fd()).unwrap_or(false);
        Self {
            ticks_per_sec,
            page_size_kb,
            have_schedstats,
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
        if thread.end_reason.is_none() {
            proc.active_threads.insert(tid);
            thread.proc = Some(proc_rc.clone());
            trace!("Process #{} {} bound to thread {}, active={}",
                proc.ordinal, proc.pid, tid, proc.active_threads.len());
        } else {
            debug!("Thread {} is already finished, not binding to process #{} {}",
                tid, proc.ordinal, proc.pid);
        }

        // if let Some(unbound_proc_rc) = &thread.orphan_proc {
        //     //if !ptr::eq(proc_rc.as_ptr(), unbound_proc_rc.as_ptr()) {
        //         let mut unbound_proc = unbound_proc_rc.borrow_mut();
        //         debug!("try bind pid {} to ppid {pid}", unbound_proc.pid);
        //         unbound_proc.parent = Some(proc_rc.clone());
        //         proc.children.push(unbound_proc_rc.clone());
        //         drop(unbound_proc);
        //         thread.orphan_proc = None;
        //     //}
        // }
        drop(proc);

        if !thread.unbound_child_tids.is_empty() {
            let grandchildren = mem::take(&mut thread.unbound_child_tids);
            for tid in grandchildren {
                self.bind_tid(tid, pid);
            }
        }
    }

    fn unbind_tid(&mut self, tid: Pid, reason: ProcessEndReason) -> Option<Rc<RefCell<ProcessSpan>>> {
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

        let end_time = Utc::now();
        let cpu_time = if self.have_schedstats {
            let filename = format!("/proc/{}/task/{}/schedstat", tid, tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let parts: Vec<&str> = proc_stat.split_whitespace().collect();
                    Duration::nanoseconds(parts[0].parse::<i64>().unwrap_or(0))
                },
                Err(e) => {
                    if !matches!(reason, ProcessEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    Duration::zero()
                },
            }
        } else {
            let filename = format!("/proc/{}/stat", tid);
            match fs::read_to_string(&filename) {
                Ok(proc_stat) => {
                    let post_parens = proc_stat.split_once(')').unwrap().1;
                    let parts: Vec<&str> = post_parens.split_whitespace().collect();
                    let user_time = parts[11].parse::<i64>().unwrap_or(0);
                    let sys_time = parts[12].parse::<i64>().unwrap_or(0);
                    Duration::milliseconds((1000.0 * (user_time + sys_time) as f64 / self.ticks_per_sec) as i64)
                },
                Err(e) => {
                    if !matches!(reason, ProcessEndReason::LateExitCode(_)) {
                        // late exit event happens *after* the thread has exited, so we cannot really expect the file to exist
                        error!("Failed to read {}: {}", filename, e);
                    }
                    Duration::zero()
                },
            }
        };
        let mut proc = proc_rc.borrow_mut();
        proc.elapsed += end_time - thread.start_time;
        proc.cpu_time += cpu_time - thread.last_cpu_time;

        thread.start_time = end_time;
        thread.last_cpu_time = cpu_time;
        proc.active_threads.remove(&tid);

        let proc_pid = proc.pid;
        if proc.active_threads.is_empty() {
            debug!("Process #{} {} pid={} finished by {:?}",
                proc.ordinal, proc.argv[0], proc.pid, reason);
            let proc_stat = fs::read_to_string(format!("/proc/{}/stat", tid))
                .expect("Failed to read /proc/[pid]/stat");
            let post_parens = proc_stat.split_once(')').unwrap().1;
            let parts: Vec<&str> = post_parens.split_whitespace().collect();

            let rss_kb = parts[21].parse::<i64>().unwrap_or(0) * self.page_size_kb;
            self.active_procs.remove(&proc_pid).unwrap();
            proc.end = Some(ProcessEnd {
                rss_kb,
                end_time,
                reason,
            });
            self.finished_procs.push(proc_rc.clone());
        } else {
            trace!("Unbound thread {} from process #{} {} due to {:?}, active={}",
                tid, proc.ordinal, proc.pid, reason, proc.active_threads.len());
        }

        drop(proc);
        let cloned_proc_rc = proc_rc.clone();
        thread.proc = None;
        Some(cloned_proc_rc)
    }

    pub(crate) fn start_thread(&mut self, tid: Pid, parent_tid: Pid, _event: i32) {
        let parent = self.active_threads.entry(parent_tid).or_insert_with(|| {
            debug!("Created orphan parent thread implicitly {} by tid={}", parent_tid, tid);
            ThreadSpan::new_orphan(parent_tid)
        });
        let proc = parent.proc.as_ref().cloned();
        if proc.is_none() {
            debug!("Thread started {} without parent tid={}", tid, parent_tid);
            parent.unbound_child_tids.push(tid);
        }
        self.active_threads.entry(tid).or_insert_with(|| ThreadSpan::new_orphan(tid));

        if let Some(proc) = &proc {
            let pid = proc.borrow().pid;
            self.bind_tid(tid, pid);
        }
    }

    pub(crate) fn start_proc(&mut self, pid: Pid, argv: Vec<String>, prev_tid: Option<Pid>) {
        let start_time = Utc::now();
        let mut born_orphan = false;
        let mut parent_proc = None;
        if let Some(prev_tid) = prev_tid {
            if let Some(parent_thread) = self.active_threads.get(&prev_tid) {
                trace!("Finishing previous process thread {} before new process {}", prev_tid, pid);
                parent_proc = self.unbind_tid(prev_tid, ProcessEndReason::Exec);
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
            cpu_time: Default::default(),
            elapsed: Default::default(),
            children: Default::default(),
            end: None,
        }));
        if let Some(parent_proc) = &parent_proc {
            parent_proc.borrow_mut().children.push(new_proc.clone());
        }

        self.active_threads.entry(pid).or_insert_with(|| ThreadSpan {
            tid: pid,
            proc: None,
            start_time,
            born_orphan,
            unbound_child_tids: Default::default(),
            end_reason: None,
            orphan_proc: if born_orphan { Some(new_proc.clone()) } else { None },
            last_cpu_time: Default::default(),
        });
        self.active_procs.insert(pid, new_proc);

        self.bind_tid(pid, pid);
    }

    pub(crate) fn finish_thread(&mut self, tid: Pid, proc_end: ProcessEndReason) -> Option<Rc<RefCell<ProcessSpan>>> {
        let finished_proc = self.unbind_tid(tid, proc_end.clone());
        if let Some(finished_proc) = &finished_proc {
            if matches!(&proc_end, ProcessEndReason::LateExitCode(_)) {
                debug!("finished thread without exit event: {} pid={}", tid, finished_proc.borrow().pid);
            }
        }
        match self.active_threads.entry(tid) {
            hash_map::Entry::Occupied(mut entry) => {
                let thread = entry.get_mut();
                if thread.born_orphan {
                    debug!("Thread {tid} born orphan, left in active threads");
                    thread.end_reason = Some(proc_end);
                } else {
                    entry.remove();
                }
            },
            hash_map::Entry::Vacant(entry) => {
                debug!("Finished thread {} died orphan, end={:?}", tid, proc_end);
                entry.insert(ThreadSpan::new_orphan(tid).with_end_reason(proc_end));
            }
        }

        finished_proc
    }

    fn print_tree(&self, proc: &ProcessSpan, indent: &mut String, postfix: &mut String, last: bool, is_root: bool) {
        let proc_end = proc.end.as_ref().unwrap();
        let elapsed = proc.elapsed();
        let cpu_pct = 100.0 * proc.cpu_time.as_seconds_f64() / elapsed.as_seconds_f64();
        let connector = if is_root { "" } else if last { "└─" } else { "├─" };

        let argv_cutoff = if self.is_tty { 100 } else { 0 };
        println!("{indent}{connector}{:<5}{postfix} {:<8} {:9.3}s {:7.1}%cpu {:<4} threads {:>6}MB  {:.argv_cutoff$}",
             format!("#{}", proc.ordinal),
             match proc_end.reason {
                 ProcessEndReason::ExitCode(code) | ProcessEndReason::LateExitCode(code) => {
                     if code == 0 { format!("[rc={}]", code).normal() } else { format!("[rc={}]", code).bright_red() }
                 },
                 ProcessEndReason::Signal(signal) => format!("[killed by {}]", signal).bright_red(),
                 ProcessEndReason::Exec => "[exec]".to_string().normal(),
             },
             elapsed.as_seconds_f64(),
             cpu_pct,
             proc.total_threads,
             proc_end.rss_kb / 1024,
             proc.argv.join(" "),
        );

        let prev_indent_len = indent.len();

        postfix.truncate(postfix.len() - 2);
        if !is_root {
            indent.push_str(if last { "  " } else { "│ " });
        }
        for (num, child) in proc.children.iter().enumerate() {
            self.print_tree(&child.borrow(), indent, postfix, num == proc.children.len() - 1, false);
        }
        postfix.push_str("  ");;
        if !is_root {
            indent.truncate(prev_indent_len);
        }
    }

    pub(crate) fn report(&self) {
        let mut procs_by_cmd: HashMap<String, Vec<Rc<RefCell<ProcessSpan>>>> = Default::default();

        for proc_rc in self.active_procs.values() {
            let proc = proc_rc.borrow();
            procs_by_cmd.entry(proc.argv[0].clone()).or_default().push(proc_rc.clone());
            error!("Active process #{} {} pid={} has {} active threads: {}",
                proc.ordinal, proc.argv[0], proc.pid, proc.active_threads.len(),
                     proc.active_threads.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
        }

        let mut total_cpu_time = Duration::zero();
        let total_elapsed = Utc::now() - self.finished_procs[0].borrow().start_time;

        let mut root = None;
        let mut orphans = Vec::new();
        for proc_rc in &self.finished_procs {
            let proc = proc_rc.borrow();
            procs_by_cmd.entry(proc.argv[0].clone()).or_default().push(proc_rc.clone());
            total_cpu_time += proc.cpu_time;
            if proc.ordinal == 1 {
                root = Some(proc_rc.clone());
            } else if proc.parent.is_none() {
                orphans.push(proc_rc.clone());
            }
        }
        let root = root.as_ref().unwrap().borrow();
        let tree_depth = root.tree_depth();
        let mut postfix = "  ".repeat(tree_depth + 1);

        self.print_tree(&root, &mut String::new(), &mut postfix, true, true);
        for (num, orphan) in orphans.iter().enumerate() {
            let orphan = orphan.borrow();
            error!("process without parent: #{} {}", orphan.ordinal, orphan.pid);
            self.print_tree(&orphan, &mut String::from("  "), &mut postfix, num == orphans.len() - 1, false);
        }

        if procs_by_cmd.values().any(|procs| procs.len() >= 3) {
            println!("\nGroup by command:");
            for (name, procs) in procs_by_cmd {
                let mut cmd_cpu_time = Duration::zero();
                let mut cmd_elapsed = Duration::zero();
                for proc_rc in &procs {
                    let proc = proc_rc.borrow();
                    cmd_elapsed += proc.elapsed();
                    cmd_cpu_time += proc.cpu_time;
                }
                println!("{:>9.3}s {:>7.1}%cpu {:>5} processes  {name}",
                    cmd_cpu_time.as_seconds_f64(),
                    100.0 * cmd_cpu_time.as_seconds_f64() / cmd_elapsed.as_seconds_f64(),
                    procs.len(),
                );
            }
        }


        println!("{}: {} processes {:7.3}s {:7.1}%cpu",
            root.argv[0],
            self.finished_procs.len(),
            total_elapsed.as_seconds_f64(),
            100.0 * total_cpu_time.as_seconds_f64() / total_elapsed.as_seconds_f64());
    }
}
