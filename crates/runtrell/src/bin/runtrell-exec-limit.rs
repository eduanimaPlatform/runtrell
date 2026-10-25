// SPDX-License-Identifier: Apache-2.0
#![deny(unsafe_op_in_unsafe_fn)]
//! Private launcher: applies hard rlimits in a fresh process, then execs confinement.
#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rustix::process::{Resource, Rlimit, setrlimit};
    use std::os::unix::process::CommandExt;
    close_inherited_descriptors()?;
    let mut args = std::env::args().skip(1);
    let cpu = args.next().ok_or("cpu")?.parse::<u64>()?;
    let file = args.next().ok_or("file")?.parse::<u64>()?;
    for (resource, value) in [
        (Resource::Cpu, cpu),
        (Resource::Fsize, file),
        (Resource::Nofile, 64),
        (Resource::Core, 0),
    ] {
        if value != u64::MAX {
            setrlimit(
                resource,
                Rlimit {
                    current: Some(value),
                    maximum: Some(value),
                },
            )?;
        }
    }
    let binary = args.next().ok_or("confinement executable")?;
    Err(std::process::Command::new(binary).args(args).exec().into())
}
#[cfg(unix)]
fn close_inherited_descriptors() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(not(target_os = "linux"))]
    let path = "/dev/fd";
    let descriptors = std::fs::read_dir(path)?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().parse::<i32>().ok()))
        .collect::<std::io::Result<Vec<_>>>()?;
    for fd in descriptors.into_iter().flatten().filter(|fd| *fd > 2) {
        // SAFETY: this fresh single-threaded helper has no Rust-owned files above stdio.
        // read_dir and its DirEntries are dropped before this loop. Inherited raw FDs
        // have no Rust owners here. Closing the already-dropped directory FD can only
        // return EBADF; libc close permits arbitrary integers. No child code has run.
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    eprintln!("unsupported OS");
    std::process::exit(1);
}
