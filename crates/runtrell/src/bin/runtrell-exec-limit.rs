// SPDX-License-Identifier: Apache-2.0
#![deny(unsafe_op_in_unsafe_fn)]
//! Private launcher: applies hard rlimits in a fresh process, then execs confinement.
#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rustix::process::{Resource, Rlimit, setrlimit};
    use std::os::unix::process::CommandExt;
    close_inherited_descriptors()?;
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("cpu or --stage")?;
    if first == "--stage" {
        #[cfg(target_os = "linux")]
        {
            setrlimit(
                Resource::Core,
                Rlimit {
                    current: Some(0),
                    maximum: Some(0),
                },
            )?;
            setrlimit(
                Resource::Nofile,
                Rlimit {
                    current: Some(64),
                    maximum: Some(64),
                },
            )?;
            let parent = args.next().ok_or("parent")?.parse::<i32>()?;
            let group = args.next().ok_or("cgroup")?;
            let control = args.next().ok_or("control")?;
            let scratch = std::path::PathBuf::from(args.next().ok_or("scratch")?);
            let record = args.next().ok_or("record")?;
            join_group(parent, &group, &control)?;
            let record: serde_json::Value = serde_json::from_slice(&std::fs::read(record)?)?;
            let files: std::collections::BTreeMap<String, Vec<u8>> =
                serde_json::from_value(record["request"]["input_files"].clone())?;
            for (name, bytes) in files {
                runtrell::data::logical_path(&name)?;
                let path = scratch.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, bytes)?;
            }
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        return Err("staging requires Linux".into());
    }
    let cpu = first.parse::<u64>()?;
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
    #[allow(unused_mut)]
    let mut binary = args.next().ok_or("confinement executable")?;
    if binary == "--cgroup" {
        #[cfg(target_os = "linux")]
        {
            let parent = args.next().ok_or("parent")?.parse::<i32>()?;
            let group = args.next().ok_or("cgroup path")?;
            let control = args.next().ok_or("control")?;
            join_group(parent, &group, &control)?;
            binary = args.next().ok_or("confinement executable")?;
        }
        #[cfg(not(target_os = "linux"))]
        return Err("cgroups require Linux".into());
    }
    Err(std::process::Command::new(binary).args(args).exec().into())
}
#[cfg(target_os = "linux")]
fn join_group(parent: i32, group: &str, control: &str) -> Result<(), Box<dyn std::error::Error>> {
    use rustix::process::{Signal, getppid, set_parent_process_death_signal};
    fn check(parent: i32) -> Result<(), Box<dyn std::error::Error>> {
        if getppid().is_none_or(|pid| rustix::process::Pid::as_raw(Some(pid)) != parent) {
            return Err("PARENT_LOST".into());
        }
        Ok(())
    }
    check(parent)?;
    set_parent_process_death_signal(Some(Signal::KILL))?;
    check(parent)?;
    let control = std::path::Path::new(control);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(control.join("join.lock"))?;
    lock.lock()?;
    if control.join("cancel").exists() {
        return Err("CANCELLED_BEFORE_JOIN".into());
    }
    check(parent)?;
    std::fs::write(std::path::Path::new(group).join("cgroup.procs"), "0")?;
    // Stop writes cancel before taking this same barrier. No delayed helper can
    // join after a confirmed-empty observation, including one orphaned pre-exec.
    Ok(())
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
