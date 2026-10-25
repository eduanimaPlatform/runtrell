// SPDX-License-Identifier: Apache-2.0
use super::State;
use crate::{Error, Result};
use rustix::fs::{
    AtFlags, CWD, FileType, FlockOperation, Mode, OFlags, flock, fstat, fsync, mkdirat, openat,
    renameat, unlinkat,
};
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::OwnedFd,
    path::{Component, Path},
    sync::Mutex,
};
/// Directory capabilities remain pinned across rename; no pathname check/open gap.
pub(super) struct Local {
    root: OwnedFd,
    gate: Mutex<()>,
}
fn err(_: impl std::fmt::Debug) -> Error {
    Error::Storage
}
fn directory(parent: impl std::os::fd::AsFd, name: &str) -> Result<OwnedFd> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(err)
}
fn regular(fd: &OwnedFd) -> Result<()> {
    let stat = fstat(fd).map_err(err)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(Error::Storage);
    }
    Ok(())
}
impl Local {
    pub fn open(root: &Path) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error::Invalid("ROOT_ABSOLUTE_REQUIRED".into()));
        }
        let mut fd = directory(CWD, "/")?;
        for c in root.components() {
            match c {
                Component::RootDir => {}
                Component::Normal(n) => {
                    let name = n.to_str().ok_or(Error::Storage)?;
                    fd = directory(&fd, name)?;
                }
                _ => return Err(Error::Storage),
            }
        }
        Ok(Self {
            root: fd,
            gate: Mutex::new(()),
        })
    }
    pub fn tx<R>(&self, key: &str, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        // A per-namespace OS lock serializes processes. The local mutex only protects
        // flock semantics for threads sharing this descriptor; it is held to open
        // an independent namespace lock FD, never while another namespace does I/O.
        let dir = {
            let _g = self.gate.lock().map_err(err)?;
            match mkdirat(&self.root, key, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(err(e)),
            };
            directory(&self.root, key)?
        };
        let lock = openat(
            &dir,
            "lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(err)?;
        regular(&lock)?;
        flock(&lock, FlockOperation::LockExclusive).map_err(err)?;
        let mut state = match openat(
            &dir,
            "state",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                regular(&fd)?;
                let mut bytes = Vec::new();
                File::from(fd).read_to_end(&mut bytes).map_err(err)?;
                serde_json::from_slice::<State>(&bytes).map_err(err)?
            }
            Err(rustix::io::Errno::NOENT) => State::default(),
            Err(e) => return Err(err(e)),
        };
        let before = serde_json::to_vec(&state).map_err(err)?;
        let result = f(&mut state)?;
        let bytes = serde_json::to_vec(&state).map_err(err)?;
        if before != bytes {
            let name = format!("stage-{}", uuid::Uuid::new_v4());
            let fd = openat(
                &dir,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(err)?;
            let mut file = File::from(fd);
            let saved = (|| {
                file.write_all(&bytes).map_err(err)?;
                file.sync_all().map_err(err)?;
                renameat(&dir, name.as_str(), &dir, "state").map_err(err)?;
                fsync(&dir).map_err(err)?;
                fsync(&self.root).map_err(err)?;
                Ok(())
            })();
            if saved.is_err() {
                let _ = unlinkat(&dir, name.as_str(), AtFlags::empty());
            }
            saved?;
        }
        Ok(result)
    }
}
