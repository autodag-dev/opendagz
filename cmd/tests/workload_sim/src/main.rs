use std::{env, process};
use std::time::Instant;

fn spin_cpu_ms(ms: u64) {
    println!("@@ spin {ms}ms");
    let deadline = Instant::now() + std::time::Duration::from_millis(ms);
    while Instant::now() < deadline {}
}

fn main() {
    // let t1_handle = thread::spawn(|| {
    //     spin_cpu_ms(100);
    // });
    // 
    // let t2_handle = thread::spawn(|| {
    //     spin_cpu_ms(100);
    // });

    if env::args().len() > 1 {
        spin_cpu_ms(100);
        process::exit(0);
    }

    // fork a process
    let p1_handle = match unsafe {nix::unistd::fork()} {
        Ok(nix::unistd::ForkResult::Parent { child }) => {
            println!("Parent process with PID: {}", nix::unistd::getpid());
            child
        }
        Ok(nix::unistd::ForkResult::Child) => {
            println!("Child process with PID: {}", nix::unistd::getpid());
            spin_cpu_ms(100);
            // if env::args().len() == 1 {
            //     println!("------------------3 --------------------");
            //     let cmd = CString::new(env::args().next().unwrap()).unwrap();
            //     nix::unistd::execv(&cmd, &[&cmd, &CString::new("3").unwrap()])
            //         .expect("Failed to exec self");
            // }
            process::exit(0);
        }
        Err(e) => {
            eprintln!("Fork failed: {}", e);
            process::exit(1);
        }
    };
    
    //t1_handle.join().unwrap();
    //t2_handle.join().unwrap();
    nix::sys::wait::waitpid(p1_handle, None).unwrap();
    spin_cpu_ms(80);
    
    // exec self
    // if env::args().len() == 1 {
    //     let cmd = CString::new(env::args().next().unwrap()).unwrap();
    //     nix::unistd::execv(&cmd, &[&cmd, &CString::new("2").unwrap()])
    //         .expect("Failed to exec self");
    // }
}
