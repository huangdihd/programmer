// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle, RawHandle};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn TerminateProcess(process: RawHandle, exit_code: u32) -> i32;
    fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
}

/// Owns a duplicate, so the waiter may drop its child without invalidating kills.
pub(super) struct PtyKiller {
    process: OwnedHandle,
}

impl PtyKiller {
    pub(super) fn new(child: &dyn portable_pty::Child) -> io::Result<Self> {
        let handle = child
            .as_raw_handle()
            .ok_or_else(|| io::Error::other("PTY child has no process handle"))?;
        // The child stays borrowed until duplication completes.
        let process = unsafe { BorrowedHandle::borrow_raw(handle) }.try_clone_to_owned()?;
        Ok(Self { process })
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        terminate_process(self.process.as_raw_handle())
    }
}

fn terminate_process(process: RawHandle) -> io::Result<()> {
    // portable-pty 0.9.0's WinChildKiller inverts this success check and returns
    // stale last_os_error on success. Do not use its clone_killer on Windows.
    if unsafe { TerminateProcess(process, 1) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // Exit can race a kill, or the waiter can still be draining PTY output.
    // Only a signaled process proves termination; all other failures surface.
    const WAIT_OBJECT_0: u32 = 0;
    if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn duplicate_survives_child_drop_and_repeated_termination() {
        let mut child = Command::new("cmd")
            .args(["/D", "/Q", "/K"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let process = unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }
            .try_clone_to_owned()
            .unwrap();
        let mut killer = PtyKiller { process };
        let _stdin = child.stdin.take().unwrap();
        drop(child);
        killer.kill().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(killer.process.as_raw_handle(), 5_000) },
            0
        );
        killer.kill().unwrap();
    }

    #[test]
    fn already_exited_process_can_be_killed() {
        let mut child = Command::new("cmd")
            .args(["/D", "/C", "exit 0"])
            .spawn()
            .unwrap();
        let process = unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }
            .try_clone_to_owned()
            .unwrap();
        child.wait().unwrap();
        drop(child);
        PtyKiller { process }.kill().unwrap();
    }

    #[test]
    fn invalid_handle_is_reported() {
        let error = terminate_process(std::ptr::null_mut()).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(6));
    }
}
