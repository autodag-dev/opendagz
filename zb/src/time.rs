use nix::sys::ptrace;
use nix::sys::wait::{WaitStatus};
use nix::unistd::{ForkResult, Pid};
use std::ffi::CString;
use std::{io, mem, process};
use std::fs::File;
use std::time::Instant;
use nix::errno::Errno;
use nix::libc;
use tracing::{debug, error, trace};
use tracing_subscriber::{fmt, prelude::*, filter::LevelFilter};
use crate::command_tree::CommandTree;
use crate::thread_tracker::{ThreadEnd, ProcessEndReason, ThreadTracker};

#[derive(clap::Args)]
pub(crate) struct TimeCommand {
    /// Show verbose output
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Output format (real, user, sys)
    #[arg(short, long, default_value = "real")]
    format: String,

    #[arg(long, help = "Debug log file")]
    log: Option<String>,
    
    #[arg(short, long, help = "Write output to file instead of stdout")]
    output: Option<String>,

    /// Command and arguments to time
    #[arg(trailing_var_arg = true, required = true)]
    args: Vec<String>,
}


impl TimeCommand {
    pub(crate) fn run_impl(&self) -> Result<(), nix::Error> {
        // use nix to create a child process
        let root_pid = match unsafe { nix::unistd::fork()? } {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                ptrace::traceme().map_err(|e| {
                    error!("Failed to trace child process: {}", e);
                    process::exit(2);
                })?;

                // Use exec to replace the child process with the command
                let args: Vec<CString> = self.args.iter().map(|arg| CString::new(arg.clone()).unwrap()).collect();
                nix::unistd::execvp(&args[0], &args)?;
                unreachable!("zb-time: Unexpected state: Failed to execvp in child process");
            }
        };

        let mut tracker = ThreadTracker::new();
        let mut root_exit_code = None;
        let mut deadline = None;

        loop {
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
                        root_exit_code = Some(status);
                        deadline = Some(Instant::now() + std::time::Duration::from_millis(200));
                    }
                }
                Ok(WaitStatus::Signaled(_pid, _signal, _)) => {
                    // let started = active_threads.remove(&pid).unwrap();
                    // finished.push(ThreadSpan::new(started, 2, &monitor)); // 2 for killed by signal
                    // if active_threads.is_empty() {
                    //     break;
                    // }
                    unimplemented!("Handling signaled processes is not implemented yet");
                }
                Ok(WaitStatus::PtraceEvent(parent_tid, _signal, event)) => {
                    let event_data = ptrace::getevent(parent_tid);
                    ptrace::cont(parent_tid, None).unwrap_or_else(|e| {
                        if e != nix::Error::ESRCH {
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
                        tracker.handle_exec(pid, self.args.clone(), None,
                                            ThreadEnd::from_rusage(ProcessEndReason::Exec, &rusage));
                    }

                    let cont_signal = if signal == nix::sys::signal::SIGTRAP { None } else { Some(signal) };
                    ptrace::cont(pid, cont_signal).unwrap_or_else(|e| {
                        if e != nix::Error::ESRCH {
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
                        root_exit_code = Some(2);
                    }
                    break;
                }
            }
        }

        if let Some(output) = &self.output {
            CommandTree::report(&mut File::create(output).expect("Failed to create output file"), &tracker);
        } else {
            CommandTree::report(&mut io::stdout(), &tracker);
        };
        process::exit(root_exit_code.unwrap_or(2));
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
                error!("Cannot run {}: {}", self.args.join(" "), e);
                process::exit(1);
            }
        }
    }
}
