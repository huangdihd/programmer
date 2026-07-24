// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::io::Write;

/// Copies text to the system clipboard. On Windows this writes UTF-16 straight
/// to the clipboard via the Win32 API; elsewhere it pipes to the platform's
/// clipboard command. If that fails, it falls back to the OSC 52 escape
/// sequence, which most modern terminals translate into a clipboard write.
/// Returns false when every method failed.
pub fn copy(text: &str) -> bool {
    #[cfg(windows)]
    {
        // The old path piped UTF-8 bytes to `clip`, which decodes stdin using
        // the console codepage (e.g. GBK/936 on Chinese Windows) — so any
        // non-ASCII text landed in the clipboard as mojibake. The Win32 API
        // takes UTF-16 directly and preserves it.
        if set_clipboard_windows(text) {
            return true;
        }
        return osc52(text).is_ok();
    }
    #[cfg(not(windows))]
    {
        for (cmd, args) in commands() {
            if pipe_to(cmd, args, text) {
                return true;
            }
        }
        osc52(text).is_ok()
    }
}

#[cfg(not(windows))]
fn commands() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

#[cfg(not(windows))]
fn pipe_to(cmd: &str, args: &[&str], text: &str) -> bool {
    use std::process::{Command, Stdio};
    let child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let written = child
        .stdin
        .take()
        .and_then(|mut stdin| stdin.write_all(text.as_bytes()).ok())
        .is_some();
    written && matches!(child.wait(), Ok(status) if status.success())
}

/// Write `text` to the Windows clipboard as `CF_UNICODETEXT` (UTF-16LE). Returns
/// false on any failure so the caller can fall back to OSC 52.
#[cfg(windows)]
fn set_clipboard_windows(text: &str) -> bool {
    use std::os::raw::c_void;

    const GMEM_MOVEABLE: u32 = 0x0002;
    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(mem: *mut c_void) -> i32;
        fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    }

    // UTF-16LE with a terminating NUL, as CF_UNICODETEXT requires.
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        let mut ok = false;
        if EmptyClipboard() != 0 {
            let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len);
            if !handle.is_null() {
                let dst = GlobalLock(handle);
                if !dst.is_null() {
                    std::ptr::copy_nonoverlapping(utf16.as_ptr(), dst as *mut u16, utf16.len());
                    GlobalUnlock(handle);
                    // On success the system owns the memory; on failure free it.
                    if !SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                        ok = true;
                    } else {
                        GlobalFree(handle);
                    }
                } else {
                    GlobalFree(handle);
                }
            }
        }
        CloseClipboard();
        ok
    }
}

fn osc52(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    stdout.flush()
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::raw::c_void;

    /// Read `CF_UNICODETEXT` back off the clipboard, for verifying [`copy`].
    fn read_clipboard() -> Option<String> {
        const CF_UNICODETEXT: u32 = 13;
        #[link(name = "user32")]
        unsafe extern "system" {
            fn OpenClipboard(hwnd: *mut c_void) -> i32;
            fn GetClipboardData(format: u32) -> *mut c_void;
            fn CloseClipboard() -> i32;
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GlobalLock(mem: *mut c_void) -> *mut c_void;
            fn GlobalUnlock(mem: *mut c_void) -> i32;
        }
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let handle = GetClipboardData(CF_UNICODETEXT);
            let result = if handle.is_null() {
                None
            } else {
                let ptr = GlobalLock(handle) as *const u16;
                if ptr.is_null() {
                    None
                } else {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    let s = String::from_utf16_lossy(slice);
                    GlobalUnlock(handle);
                    Some(s)
                }
            };
            CloseClipboard();
            result
        }
    }

    #[test]
    fn copy_roundtrips_unicode_without_mojibake() {
        // The exact failure the user hit: non-ASCII went through `clip` and came
        // back garbled. A native UTF-16 write must preserve it byte-for-byte.
        let text = "无法立即完成一个非阻止性套接字操作 — hello 世界 🌍";
        assert!(set_clipboard_windows(text), "native clipboard write should succeed");
        assert_eq!(read_clipboard().as_deref(), Some(text));
    }
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
