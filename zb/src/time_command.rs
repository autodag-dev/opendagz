use nix::sys::ptrace;
use nix::sys::wait::{WaitStatus};
use nix::unistd::{ForkResult, Pid};
use std::ffi::CString;
use std::{io, mem, process};
use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use nix::errno::Errno;
use nix::libc;
use tracing::{debug, error, trace};
use tracing_subscriber::{fmt, prelude::*, filter::LevelFilter};
use crate::command_tree::CommandTree;
use crate::thread_tracker::{ThreadEnd, ProcessEndReason, ThreadTracker};

/// Run a command and show its process tree and metrics.
#[derive(clap::Args)]
pub(crate) struct TimeCommand {
    /// Show verbose output (-v | -vv | -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Write detailed logs to file
    #[arg(long)]
    log: Option<String>,
    
    #[arg(short, long, help = "Write output to file instead of stdout")]
    output: Option<String>,

    /// Command and arguments to run
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}


impl TimeCommand {
    pub(crate) fn run_impl(&self) -> Result<(), io::Error> {
        // use nix to create a child process
        let root_pid = match unsafe { nix::unistd::fork()? } {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                ptrace::traceme()?;

                // Use exec to replace the child process with the command
                let args: Vec<CString> = self.command.iter().map(|arg| CString::new(arg.clone()).unwrap()).collect();
                nix::unistd::execvp(&args[0], &args)?;
                unreachable!("zb-time: Unexpected state: Failed to execvp in child process");
            }
        };

        // set signal handlers
        let terminate_flag = Arc::new(AtomicBool::new(false));
        for sig in signal_hook::consts::TERM_SIGNALS {
            signal_hook::flag::register_conditional_shutdown(*sig, 2, terminate_flag.clone())?;
            signal_hook::flag::register(*sig, Arc::clone(&terminate_flag))?;
        }

        let mut tracker = ThreadTracker::new();
        let mut root_end = None;
        let mut deadline = None;

        loop {
            if terminate_flag.load(Ordering::Relaxed) && deadline.is_none() {
                eprintln!("zb-time: Caught termination signal, command should exit now...\n");
                deadline = Some(Instant::now() + Duration::from_secs(3));
            }

            if let Some(deadline) = deadline {
                if Instant::now() >= deadline {
                    error!("Timeout reached after command exit, some processes may still be running");
                    break;
                }
            }
            let (wait_result, rusage) = unsafe {
                let mut status: libc::c_int = 0;
                let mut rusage: libc::rusage = mem::zeroed();
                let res = libc::wait4(-1 as libc::pid_t, &mut status, libc::__WALL, &mut rusage);

                (match Errno::result(res) {
                    Ok(0) => Ok(WaitStatus::StillAlive),
                    Ok(res) => WaitStatus::from_raw(Pid::from_raw(res), status),
                    Err(e) => Err(e),
                }, rusage)
            };

            match wait_result {
                Ok(WaitStatus::Exited(tid, status)) => {
                    trace!("waitpid result exit: {:?}", wait_result);
                    tracker.finish_thread(tid, ThreadEnd::from_rusage(ProcessEndReason::LateExitCode(status), &rusage));
                    if tid == root_pid {
                        root_end = Some(ProcessEndReason::ExitCode(status));
                        deadline = Some(Instant::now() + Duration::from_millis(200));
                    }
                }
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    error!("Thread {pid} killed by {signal}");
                    if pid == root_pid {
                        root_end = Some(ProcessEndReason::Signal(signal));
                    }
                    tracker.finish_thread(pid, ThreadEnd::from_rusage(ProcessEndReason::Signal(signal), &rusage));
                }
                Ok(WaitStatus::PtraceEvent(parent_tid, _signal, event)) => {
                    let event_data = ptrace::getevent(parent_tid);
                    ptrace::cont(parent_tid, None).unwrap_or_else(|e| {
                        if e == nix::Error::ESRCH {
                            debug!("Process {} already exited, cannot continue", parent_tid);
                        } else {
                            error!("Failed to continue process {}: {}", parent_tid, e);
                        }
                    });

                    trace!("waitpid result: {:?} data={}", wait_result, event_data.unwrap_or(-1));
                    match event {
                        libc::PTRACE_EVENT_FORK | libc::PTRACE_EVENT_VFORK | libc::PTRACE_EVENT_CLONE => {
                            match event_data {
                                Ok(new_pid) => {
                                    let new_tid = Pid::from_raw(new_pid as libc::pid_t);
                                    let is_fork = event == libc::PTRACE_EVENT_FORK || event == libc::PTRACE_EVENT_VFORK;
                                    tracker.handle_spawn(new_tid, parent_tid, is_fork);
                                }
                                Err(e) => {
                                    error!("Failed to get new child PID from ptrace event: {}", e);
                                }
                            };
                        },
                        libc::PTRACE_EVENT_EXEC => {
                            match event_data {
                                Ok(prev_tid) => {
                                    let prev_tid = Pid::from_raw(prev_tid as libc::pid_t);

                                    // read /proc/[pid]/cmdline to get the command name
                                    let argv = std::fs::read_to_string(format!("/proc/{}/cmdline", parent_tid))
                                        .unwrap_or_else(|_| "unknown".to_string())
                                        .split('\0')
                                        .map(|s| s.to_string())
                                        .collect::<Vec<_>>();
                                    tracker.handle_exec(parent_tid, argv, Some(prev_tid), ThreadEnd::from_rusage(
                                        ProcessEndReason::Exec,
                                        &rusage,
                                    ));
                                }
                                Err(e) => {
                                    error!("Failed to get new child PID from ptrace event: {}", e);
                                }
                            };
                        },
                        libc::PTRACE_EVENT_EXIT => {
                            match event_data {
                                Ok(exit_code) => {
                                    tracker.finish_thread(parent_tid, ThreadEnd::from_rusage(
                                        ProcessEndReason::ExitCode(exit_code as i32 >> 8), &rusage));
                                }
                                Err(e) => {
                                    error!("Failed to get new child PID from ptrace event: {}", e);
                                }
                            };
                        }
                        libc::PTRACE_EVENT_VFORK_DONE => {
                            trace!("VFORK_DONE event received for parent_tid {} new_pid={}", parent_tid, event_data.unwrap_or(-1));
                        }

                        _ => {
                            error!("Unhandled ptrace event: {:?}", wait_result);
                        }
                    }
                }
                Ok(WaitStatus::Stopped(pid, signal)) => {
                    if tracker.threads.is_empty() {
                        // first child
                        ptrace::setoptions(
                            pid,
                            ptrace::Options::PTRACE_O_TRACEFORK
                                | ptrace::Options::PTRACE_O_TRACEVFORK
                                | ptrace::Options::PTRACE_O_TRACECLONE
                                | ptrace::Options::PTRACE_O_TRACEEXEC
                                | ptrace::Options::PTRACE_O_TRACEEXIT,
                        )
                        .unwrap_or_else(|e| {
                            error!("Failed to set ptrace options for child process {}: {}", pid, e)
                        });
                        tracker.handle_exec(pid, self.command.clone(), None,
                                            ThreadEnd::from_rusage(ProcessEndReason::Exec, &rusage));
                    }

                    let cont_signal = if signal == nix::sys::signal::SIGTRAP { None } else { Some(signal) };
                    ptrace::cont(pid, cont_signal).unwrap_or_else(|e| {
                        if e == nix::Error::ESRCH {
                            debug!("Process {} already exited, cannot continue", pid);
                        } else {
                            error!("Failed to continue process {}: {}", pid, e);
                        }
                    });
                    if signal != nix::sys::signal::SIGSTOP {
                        trace!("thread {} stop with signal {}", pid, signal);
                    }
                }
                Ok(_) => {
                    error!("unexpected waitpid event {:?}", wait_result);
                }
                Err(e) => {
                    if e == Errno::ECHILD {
                        // No more child processes
                        debug!("waitpid: No more child processes");
                    } else {
                        error!("waitpid error: {}", e);
                        root_end = Some(ProcessEndReason::ExitCode(2));
                    }
                    break;
                }
            }
        }

        if let Some(output) = &self.output {
            CommandTree::report(&mut File::create(output).expect("Failed to create output file"), &tracker, root_end.clone());
        } else {
            CommandTree::report(&mut io::stdout(), &tracker, root_end.clone());
        };
        process::exit(match root_end {
            Some(ProcessEndReason::ExitCode(code)) => code,
            Some(ProcessEndReason::LateExitCode(code)) => code,
            _ => 2
        });
    }

    pub(crate) fn run(&self) {
        let console_level = match self.verbose { 
            0 => LevelFilter::WARN,
            1 => LevelFilter::INFO,
            2 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        };
        let console_layer = fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(true)
            .with_timer(fmt::time::uptime()) // Shows time since program start
            .with_filter(console_level);

        let tracing_builder = tracing_subscriber::registry().with(console_layer);

        if let Some(log) = &self.log {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(File::create(log).expect("Failed to create log file"))
                .with_ansi(true)
                .with_filter(LevelFilter::TRACE);

                tracing_builder.with(file_layer).init();
        } else {
            tracing_builder.init();
        }

        match self.run_impl() {
            Ok(_) => (),
            Err(e) => {
                error!("Cannot run {}: {}", self.command.join(" "), e);
                process::exit(1);
            }
        }
    }
}
