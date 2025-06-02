use nix::sys::ptrace;
use nix::sys::wait::{WaitPidFlag, WaitStatus};
use nix::unistd::{ForkResult, Pid};
use std::ffi::CString;
use std::{process};
use std::fs::File;
use nix::libc;
use tracing::{debug, error, trace, Level};
//use tracing_subscriber::filter::LevelFilter;
//use tracing_subscriber::fmt::writer::MakeWriterExt;
//use tracing_subscriber::layer::SubscriberExt;
//use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, prelude::*, filter::LevelFilter};
use tracing_subscriber::fmt::{writer::MakeWriterExt};
use crate::thread_monitor::{ProcessEndReason, ThreadMonitor};

#[derive(clap::Args)]
pub(crate) struct TimeCommand {
    /// Show verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Output format (real, user, sys)
    #[arg(short, long, default_value = "real")]
    format: String,

    #[arg(long, help = "Debug log file")]
    log: Option<String>,

    /// Command and arguments to time
    #[arg(trailing_var_arg = true, required = true)]
    args: Vec<String>,
}

impl TimeCommand {
    fn cont(&self, pid: Pid) {
        ptrace::cont(pid, None).unwrap_or_else(|e| {
            if e != nix::Error::ESRCH {
                debug!("Process {} already exited, cannot continue", pid);
            } else {
                error!("Failed to continue process {}: {}", pid, e);
            }
        });
    }
    
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

        let mut monitor = ThreadMonitor::new();
        let mut root_exit_code = None;

        loop {
            let wait_result = nix::sys::wait::waitpid(None, WaitPidFlag::__WALL.into());
            match wait_result {
                Ok(WaitStatus::Exited(tid, status)) => {
                    trace!("waitpid result exit: {:?}", wait_result);
                    monitor.finish_thread(tid, ProcessEndReason::LateExitCode(status));
                    if monitor.active_procs.is_empty() {
                        debug!("all child processes have exited, expecting last wait");
                    }
                    if tid == root_pid {
                        root_exit_code = Some(status);
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
                    self.cont(parent_tid);
                    trace!("waitpid result: {:?} data={}", wait_result, event_data.unwrap_or(-1));
                    match event {
                        libc::PTRACE_EVENT_FORK | libc::PTRACE_EVENT_VFORK | libc::PTRACE_EVENT_CLONE => {
                            match event_data {
                                Ok(new_pid) => {
                                    let new_tid = Pid::from_raw(new_pid as libc::pid_t);
                                    monitor.start_thread(new_tid, parent_tid, event);
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
                                    monitor.start_proc(parent_tid, argv, Some(prev_tid));
                                }
                                Err(e) => {
                                    error!("Failed to get new child PID from ptrace event: {}", e);
                                }
                            };
                        },
                        libc::PTRACE_EVENT_EXIT => {
                            match event_data {
                                Ok(exit_code) => {
                                    monitor.finish_thread(parent_tid, ProcessEndReason::ExitCode(exit_code as i32 >> 8));
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
                    if monitor.active_procs.is_empty() {
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
                        monitor.start_proc(pid, self.args.clone(), None);
                    }
                    self.cont(pid);
                    if signal != nix::sys::signal::SIGUSR1 {
                        trace!("thread {} stop with signal {}", pid, signal);
                    }
                }
                Ok(_) => {
                    error!("unexpected waitpid event {:?}", wait_result);
                }
                Err(e) => {
                    if e == nix::errno::Errno::ECHILD {
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

        monitor.report();
        process::exit(root_exit_code.unwrap_or(2));
    }

    pub(crate) fn run(&self) {
        let console_layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_filter(LevelFilter::WARN);

        let tracing_builder = tracing_subscriber::registry()
            .with(console_layer);

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
