use std::cell::RefCell;
use std::{fmt, io};
use std::io::stdout;
use std::os::fd::AsFd;
use std::rc::Rc;
use std::time::{Duration, Instant};
use colored::Colorize;
use tracing::debug;
use crate::thread_tracker::{ProcessEndReason, ResourceUsage, ThreadInit, ThreadSpan, ThreadTracker};

pub(crate) struct CommandSpan {
    ordinal: usize,
    tree_depth: usize,
    lead: Rc<RefCell<ThreadSpan>>,
    children: Vec<Rc<RefCell<CommandSpan>>>,
}

pub fn format_elapsed<W: fmt::Write>(elapsed: chrono::Duration, w: &mut W) -> fmt::Result {
    let elapsed = elapsed.num_milliseconds();
    let (num_secs, msec) = (elapsed / 1000, elapsed % 1000);
    let (num_mins, sec) = (num_secs / 60, num_secs % 60);
    let (num_hours, min) = (num_mins / 60, num_mins % 60);
    let (num_days, hours) = (num_hours / 24, num_hours % 24);
    if num_days > 0 {
        write!(w, "{:03}d {:02}:{:02}:{:02}.{:03} ", num_days, hours, min, sec, msec)
    } else {
        write!(w, "{:02}:{:02}:{:02}.{:03} ", hours, min, sec, msec)
    }
}

impl CommandSpan {
    fn collect_commands(commands: &mut Vec<Rc<RefCell<CommandSpan>>>, next_ordinal: &mut usize, thread: &ThreadSpan, tree_depth: &mut usize) {
        for child_rc in &thread.children {
            let child = child_rc.borrow();
            if matches!(child.init, ThreadInit::Exec(_)) {
                let new_command = CommandSpan::new(child_rc.clone(), next_ordinal);
                *tree_depth = (*tree_depth).max(new_command.tree_depth + 1);
                commands.push(Rc::new(RefCell::new(new_command)));
            } else {
                Self::collect_commands(commands, next_ordinal, &child, tree_depth);
            }
        }
    }

    fn new(lead: Rc<RefCell<ThreadSpan>>, next_ordinal: &mut usize) -> Self {
        let mut children = Vec::new();
        let ordinal = *next_ordinal;
        *next_ordinal += 1;
        let mut tree_depth = 1;
        Self::collect_commands(&mut children, next_ordinal, &lead.borrow(), &mut tree_depth);
        debug!("new command #{}: lead=#{} {} {:?}",
            ordinal, lead.borrow().ordinal, lead.borrow().tid, lead.borrow().init);
        Self {
            ordinal,
            tree_depth,
            lead,
            children,
        }
    }
}

#[derive(Default)]
struct ProcessGroup {
    num_execs: usize,
    total_rss_kb: i64,
    max_rss_kb: i64,
    total_elapsed: Duration,
    total_usage: ResourceUsage,
}

impl ProcessGroup {
    fn add(&mut self, proc: Rc<RefCell<CommandSpan>>) {
        let proc = proc.borrow();
        let lead = proc.lead.borrow();
        let rss_kb = if let Some(end) = lead.end.as_ref() { end.usage.max_rss_kb } else { 0 };
        self.num_execs += 1;
        self.total_rss_kb += rss_kb;
        self.max_rss_kb = self.max_rss_kb.max(rss_kb);
        self.total_elapsed += proc.elapsed();
        if let Some(end) = lead.end.as_ref() {
            self.total_usage.add(&end.usage);
        }
    }
}

impl CommandSpan {
    pub(crate) fn elapsed(&self) -> Duration {
        let lead = self.lead.borrow();
        lead.tree_end_time - lead.start_time
    }
}

pub(crate) struct CommandTree {
    is_tty: bool,
    start_time: Instant,
    root: Rc<RefCell<CommandSpan>>,
}

impl CommandTree {
    pub(crate) fn new(root: Rc<RefCell<ThreadSpan>>) -> Self {
        let root = CommandSpan::new(root.clone(), &mut 1);
        let start_time = root.lead.borrow().start_time;
        let root = Rc::new(RefCell::new(root));

        let is_tty = nix::unistd::isatty(stdout().as_fd()).unwrap_or(false);
        Self {
            start_time,
            is_tty,
            root,
        }
    }

    fn print_tree(&self, output: &mut dyn io::Write, cmd: &CommandSpan, indent: &mut String, postfix: &mut String, last: bool, is_root: bool) {
        let lead = cmd.lead.borrow();
        let cmd_end = lead.end.as_ref().unwrap();
        let elapsed = cmd.elapsed();
        let connector = if is_root { "" } else if last { "└─" } else { "├─" };

        let argv_cutoff = if self.is_tty { 100 } else { 0 };
        let argv = if let ThreadInit::Exec(argv) = &lead.init { argv.join(" ") } else { "ERROR: missing argv".into() };
        let mut cmd_usage = lead.tree_usage.clone();
        cmd_usage.sub(&lead.children_usage);
        let mut start_time = String::new();
        format_elapsed(chrono::Duration::from_std(lead.start_time - self.start_time).unwrap(), &mut start_time).unwrap();

        writeln!(output, "{indent}{connector}{:<5}{postfix} {} {:9.3}s {:7.1}%cpu (tree: {:7.1}%cpu) {:4} MB {:>4} threads {:>8} {:.argv_cutoff$}",
             format!("#{}", cmd.ordinal),
             start_time,
             elapsed.as_secs_f64(),
             100.0 * cmd_usage.cpu().as_seconds_f64() / elapsed.as_secs_f64(),
             100.0 * lead.tree_usage.cpu().as_seconds_f64() / elapsed.as_secs_f64(),
             cmd_usage.max_rss_kb / 1024,
             cmd_usage.threads,
             match cmd_end.reason {
                 ProcessEndReason::ExitCode(code) | ProcessEndReason::LateExitCode(code) => {
                     if code == 0 { format!("[rc={}]", code).normal() } else { format!("[rc={}]", code).bright_red() }
                 },
                 ProcessEndReason::Signal(signal) => format!("[killed by {}]", signal).bright_red(),
                 ProcessEndReason::Exec => "[exec]".to_string().normal(),
             },
             argv,
        ).expect("Failed to write to output");

        let prev_indent_len = indent.len();

        postfix.truncate(postfix.len() - 2);
        if !is_root {
            indent.push_str(if last { "  " } else { "│ " });
        }
        for (num, child) in cmd.children.iter().enumerate() {
            self.print_tree(output, &child.borrow(), indent, postfix, num == cmd.children.len() - 1, false);
        }
        postfix.push_str("  ");
        if !is_root {
            indent.truncate(prev_indent_len);
        }
    }
    
    pub(crate) fn report(output: &mut dyn io::Write, tracker: &ThreadTracker) {
        let root_thread = tracker.root.as_ref().unwrap();
        root_thread.borrow_mut().compile_tree(0);
        let tree = CommandTree::new(root_thread.clone());
        let root = tree.root.borrow();
        let mut postfix = "  ".repeat(root.tree_depth);
        tree.print_tree(output, &root, &mut String::new(), &mut postfix, true, true);
    }

    // pub(crate) fn report(&self, output: &mut dyn io::Write) {
    //     let mut proc_groups: HashMap<String, ProcessGroup> = Default::default();
    //     let tree_depth = root.compile_tree();
    //     drop(root);
    //     let root = self.root.as_ref().unwrap().borrow();
    // 
    //     for proc_rc in self.procs.values() {
    //         let proc = proc_rc.borrow();
    //         proc_groups.entry(proc.argv[0].clone()).or_default().add(proc_rc.clone());
    //         error!("Active process #{} {} pid={} has {} active threads: {}",
    //             proc.ordinal, proc.argv[0], proc.pid, proc.active_threads.len(),
    //                  proc.active_threads.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
    //     }
    // 
    //     let mut total_cpu_time = Duration::default();
    //     let total_elapsed = Instant::now() - root.start_time;
    // 
    //     let mut orphans = Vec::new();
    //     for proc_rc in &self.finished {
    //         let proc = proc_rc.borrow();
    //         proc_groups.entry(proc.argv[0].clone()).or_default().add(proc_rc.clone());
    //         total_cpu_time += proc.self_cpu_time;
    //         if proc.ordinal != 1 && proc.spawner.is_none() {
    //             error!("process without parent: #{} {}", proc.ordinal, proc.pid);
    //             orphans.push(proc_rc.clone());
    //         }
    //     }
    //     let mut postfix = "  ".repeat(tree_depth + 1);
    // 
    //     self.print_tree(output, &root, &mut String::new(), &mut postfix, true, true);
    //     for (num, orphan) in orphans.iter().enumerate() {
    //         let orphan = orphan.borrow();
    //         self.print_tree(output, &orphan, &mut String::from("  "), &mut postfix, num == orphans.len() - 1, false);
    //     }
    // 
    //     if proc_groups.values().any(|group| group.num_execs >= 3) {
    //         writeln!(output, "\nGroup by command:").expect("Failed to write to output");
    //         let mut proc_groups = proc_groups.iter().collect::<Vec<_>>();
    //         proc_groups.sort_by_key(|(_, group)| group.total_self_cpu_time);
    //         for (name, group) in proc_groups {
    //             writeln!(output, "{:>9.3}s {:>7.1}%cpu {:4} (tree: {:7.1}%cpu) MB avg {:4} MB max {:>5} execs  {name}",
    //                      group.total_self_cpu_time.as_secs_f64(),
    //                      100.0 * group.total_self_cpu_time.as_secs_f64() / group.total_elapsed.as_secs_f64(),
    //                      100.0 * group.total_tree_cpu_time.as_secs_f64() / group.total_elapsed.as_secs_f64(),
    //                      group.total_rss_kb / 1024 / group.num_execs as i64,
    //                      group.max_rss_kb / 1024,
    //                      group.num_execs,
    //             ).expect("Failed to write to output");
    //         }
    //     }
    // 
    //     writeln!(output, "{}: {} processes {:7.3}s {:7.1}%cpu",
    //              root.argv[0],
    //              self.finished.len(),
    //              total_elapsed.as_secs_f64(),
    //              100.0 * total_cpu_time.as_secs_f64() / total_elapsed.as_secs_f64()
    //     ).expect("Failed to write to output");
    // 
    // }
}
