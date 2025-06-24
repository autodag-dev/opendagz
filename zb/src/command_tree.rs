use std::cell::RefCell;
use std::{fmt, io};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use colored::Colorize;
use tracing::{debug};
use crate::thread_tracker::{ThreadEndReason, ResourceUsage, ThreadInit, ThreadSpan, ThreadTracker};

pub(crate) struct CommandSpan {
    ordinal: usize,
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

#[derive(Default)]
struct CommandGroup {
    num_execs: usize,
    total_rss_kb: i64,
    max_rss_kb: i64,
    total_elapsed: Duration,
    total_self_usage: ResourceUsage,
    total_tree_usage: ResourceUsage,
}

impl CommandGroup {
    fn add(&mut self, cmd: &CommandSpan) {
        let lead = cmd.lead.borrow();
        let rss_kb = lead.usage.max_rss_kb;
        self.num_execs += 1;
        self.total_rss_kb += rss_kb;
        self.max_rss_kb = self.max_rss_kb.max(rss_kb);
        self.total_elapsed += cmd.elapsed();
        self.total_self_usage.add_all(&lead.usage);
        self.total_tree_usage.add_all(&lead.tree_usage);
    }
}

impl CommandSpan {
    pub(crate) fn elapsed(&self) -> Duration {
        let lead = self.lead.borrow();
        lead.end_time - lead.start_time
    }
}

pub(crate) struct CommandTree {
    is_tty: bool,
    start_time: Instant,
    elapsed: Duration,
    num_commands: usize,
    depth: usize,
    groups: HashMap<String, CommandGroup>,
}

impl CommandTree {
    pub(crate) fn new(root: Rc<RefCell<ThreadSpan>>, is_tty: bool) -> (Self, CommandSpan) {
        let start_time = root.borrow().start_time;
        let elapsed = root.borrow().end_time - start_time;

        let mut tree = Self {
            start_time,
            elapsed,
            is_tty,
            num_commands: 0,
            depth: 0,
            groups: Default::default()
        };
        let root = if let ThreadInit::Exec(argv) = &root.borrow().init {
            tree.create_command(root.clone(), argv, 1)
        } else {
            panic!("Root command without argv");
        };
        (tree, root)
    }

    fn collect_commands(&mut self, commands: &mut Vec<Rc<RefCell<CommandSpan>>>, thread: &ThreadSpan, depth: usize) {
        for child_rc in &thread.children {
            let child = child_rc.borrow();
            if let ThreadInit::Exec(argv) = &child.init {
                let new_command = self.create_command(child_rc.clone(), argv, depth);
                commands.push(Rc::new(RefCell::new(new_command)));
            } else {
                self.collect_commands(commands, &child, depth);
            }
        }
    }

    fn create_command(&mut self, lead: Rc<RefCell<ThreadSpan>>, argv: &[String], depth: usize) -> CommandSpan {
        let mut children = Vec::new();
        self.num_commands += 1;
        let ordinal = self.num_commands;
        self.collect_commands(&mut children, &lead.borrow(), depth + 1);
        debug!("new command #{}: lead=#{} {} {:?}",
            ordinal, lead.borrow().ordinal, lead.borrow().tid, lead.borrow().init);
        let cmd = CommandSpan {
            ordinal,
            lead,
            children,
        };
        self.depth = self.depth.max(depth);
        let argv0 = argv[0].as_str();
        let argv0 = match argv0.rsplit_once('/') {
            Some(argv0) => argv0.1,
            None => argv0,
        };
        
        let cmd_type = if matches!(argv0, "env" | "zig" | "time" | "cargo" | "bash" | "sh") || argv0.starts_with("python") {
            let mut i = 1;
            let argv1 = loop {
                if i >= argv.len() {
                    break ""
                }
                if argv0 == "sh" || argv0 == "bash" {
                    if argv[i] == "-c" {
                        break "";
                    }
                }
                if !argv[i].starts_with('-') {
                    break argv[i].split_whitespace().next().unwrap_or("")
                }
                if argv[i] == "-C" {
                    i += 1;
                }
                i += 1;
            };
            format!("{} {}", argv[0], argv1)
        } else {
            argv[0].clone()
        };
        self.groups.entry(cmd_type).or_default().add(&cmd);
        cmd
    }

    fn print_tree(&self, output: &mut dyn io::Write, cmd: &CommandSpan, indent: &mut String, postfix: &mut String, last: bool, is_root: bool) {
        let lead = cmd.lead.borrow();
        let end_reason = lead.end_reason.as_ref().unwrap().clone();
        let elapsed = cmd.elapsed();
        let connector = if is_root { "" } else if last { "└─" } else { "├─" };

        let argv_cutoff = if self.is_tty { 100 } else { 60000 };
        let argv = if let ThreadInit::Exec(argv) = &lead.init { argv.join(" ") } else { "ERROR: missing argv".into() };
        let cmd_usage = &lead.usage;
        let mut start_time = String::new();
        format_elapsed(chrono::Duration::from_std(lead.start_time - self.start_time).unwrap(), &mut start_time).unwrap();

        writeln!(output, "{indent}{connector}{:<5}{postfix} {} {:9.3}s {:7.1}%cpu (tree: {:7.1}%cpu) {:4} MB {:>8} iops {:4} PF {:>4} threads {:>8} {:.argv_cutoff$}",
            format!("#{}", cmd.ordinal),
            start_time,
            elapsed.as_secs_f64(),
            100.0 * cmd_usage.cpu().as_seconds_f64() / elapsed.as_secs_f64(),
            100.0 * lead.tree_usage.cpu().as_seconds_f64() / elapsed.as_secs_f64(),
            cmd_usage.max_rss_kb / 1024,
            cmd_usage.format_iops(),
            cmd_usage.major_pf,
            lead.usage.threads,
            match end_reason {
                ThreadEndReason::ExitCode(code) | ThreadEndReason::LateExitCode(code) => {
                    if code == 0 { format!("[rc={}]", code).normal() } else { format!("[rc={}]", code).bright_red() }
                },
                ThreadEndReason::Signal(signal) => format!("[killed by {}]", signal).bright_red(),
                ThreadEndReason::Exec => "[exec]".to_string().normal(),
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

    fn print_groups(&self, output: &mut dyn io::Write) {
        if self.groups.values().any(|group| group.num_execs >= 3) {
            writeln!(output, "\nGroup by command (most cpu-intensive last):").expect("Failed to write to output");
            let mut proc_groups = self.groups.iter().collect::<Vec<_>>();
            proc_groups.sort_by_key(|(_, group)| group.total_self_usage.cpu());
            for (name, group) in proc_groups {
                writeln!(output, "{:>9.3}s {:>7.1}%cpu (tree: {:7.1}%cpu) {:4} MB avg {:4} MB max {:>10} iops {:>5} execs  {name}",
                         group.total_self_usage.cpu().as_seconds_f64(),
                         100.0 * group.total_self_usage.cpu().as_seconds_f64() / group.total_elapsed.as_secs_f64(),
                         100.0 * group.total_tree_usage.cpu().as_seconds_f64() / group.total_elapsed.as_secs_f64(),
                         group.total_rss_kb / 1024 / group.num_execs as i64,
                         group.max_rss_kb / 1024,
                         group.total_self_usage.format_iops(),
                         group.num_execs
                ).expect("Failed to write to output");
            }
        }
    }
    
    fn print_summary(&self, output: &mut dyn io::Write, root: &CommandSpan, end: Option<ThreadEndReason>) {
        let root_lead = root.lead.borrow();
        if let ThreadInit::Exec(argv) = &root_lead.init {
            writeln!(output, "\n{}: {} commands {:7.3}s {:7.1}%cpu {:>12} iops {:>6} PF  {}",
                 argv[0],
                 self.num_commands,
                 self.elapsed.as_secs_f64(),
                 100.0 * root_lead.tree_usage.cpu().as_seconds_f64() / self.elapsed.as_secs_f64(),
                 root_lead.tree_usage.format_iops(),
                 root_lead.tree_usage.major_pf,
                 match end {
                     None => "Still running".normal(),
                     Some(ThreadEndReason::ExitCode(code)) | Some(ThreadEndReason::LateExitCode(code)) => {
                         if code == 0 {
                             format!("Exited {code}").normal()
                         } else {
                             format!("Exited {code}").bright_red()
                         }
                     },
                     Some(ThreadEndReason::Signal(signal)) => format!("Killed by {signal}").bright_red(),
                     Some(ThreadEndReason::Exec) => "Unknown termination reason".bright_red(),
                }
            ).expect("Failed to write to output");
        }
    }

    pub(crate) fn report(output: &mut dyn io::Write, tracker: &ThreadTracker, end: Option<ThreadEndReason>, is_tty: bool) {
        let root_thread = tracker.root.as_ref().unwrap();
        root_thread.borrow_mut().compile_tree(0);
        let (tree, root) = CommandTree::new(root_thread.clone(), is_tty);
        let mut postfix = "  ".repeat(tree.depth);
        tree.print_tree(output, &root, &mut String::new(), &mut postfix, true, true);
        tree.print_groups(output);
        if !tracker.have_schedstats {
            eprintln!("** schedstats are not enabled in the kernel, CPU measurements may be skewed");
        }
        tree.print_summary(output, &root, end);
    }
}
