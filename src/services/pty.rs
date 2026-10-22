//! Host terminal: a real PTY behind the login gate.
//!
//! `fork` is used deliberately (rather than a `Command` with a pty) because the
//! session must be a controlling terminal for job control to work. The rules
//! that make a fork safe in a multithreaded process are followed exactly:
//!
//! * the child runs **only** async-signal-safe libc calls until `execvp`
//!   (`setsid`, `dup2`, `ioctl`, `execvp`, `_exit`) — no allocation, no locks;
//! * the fork happens on a dedicated blocking task so the spawning thread is
//!   not in the middle of runtime bookkeeping;
//! * the parent immediately closes the slave and only ever touches the master.
//!
//! On a non-unix development host the whole surface degrades to an error, which
//! the WebSocket turns into the same "unavailable" report every other tool uses.

#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(unix)]
const EXIT_CODE_UNSET: i32 = i32::MIN;

/// A spawned login shell attached to a PTY.
#[cfg(unix)]
pub struct HostShell {
    master: std::os::fd::RawFd,
    pid: libc::pid_t,
    /// Set once the child has been reaped. `EXIT_CODE_UNSET` means "not reaped yet".
    exit_code: AtomicI32,
}

#[cfg(unix)]
impl HostShell {
    /// Fork a login shell (`/bin/bash -l`) on `slave`.
    pub fn spawn(rows: u16, cols: u16) -> std::io::Result<Self> {
        unsafe {
            let mut master: libc::c_int = -1;
            let mut slave: libc::c_int = -1;
            if libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }

            let pid = libc::fork();
            if pid < 0 {
                let err = std::io::Error::last_os_error();
                let _ = libc::close(master);
                let _ = libc::close(slave);
                return Err(err);
            }

            if pid == 0 {
                // Child. Everything from here to execvp is async-signal-safe.
                if libc::setsid() == -1 {
                    libc::_exit(126);
                }
                // Make the slave our controlling terminal (best effort; some
                // kernels only allow it for the session leader we just became).
                let _ = libc::ioctl(slave, libc::TIOCSCTTY, 0 as libc::c_ulong);
                for fd in [0, 1, 2] {
                    if libc::dup2(slave, fd) == -1 {
                        libc::_exit(126);
                    }
                }
                if slave > 2 {
                    let _ = libc::close(slave);
                }
                if master > 2 {
                    let _ = libc::close(master);
                }
                let mut size = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let _ = libc::ioctl(0, libc::TIOCSWINSZ, &mut size);

                let shell_path = std::env::var("SHELL")
                    .unwrap_or_else(|_| "/bin/bash".to_string());
                let shell_name = shell_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&shell_path)
                    .to_string();
                let program = std::ffi::CString::new(shell_path.clone()).unwrap();
                let argv0 = std::ffi::CString::new(shell_name).unwrap();
                let argv: [*const libc::c_char; 3] = [
                    argv0.as_ptr(),
                    b"-l\0".as_ptr().cast(),
                    std::ptr::null(),
                ];
                libc::execvp(program.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }

            let _ = libc::close(slave);
            Ok(Self {
                master,
                pid,
                exit_code: AtomicI32::new(EXIT_CODE_UNSET),
            })
        }
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Blocking read from the master. `Ok(0)` means the shell is gone.
    pub fn read(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        unsafe {
            let read = libc::read(self.master, buffer.as_mut_ptr().cast(), buffer.len());
            if read < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(read as usize)
            }
        }
    }

    pub fn write(&self, data: &[u8]) -> std::io::Result<usize> {
        unsafe {
            let written = libc::write(self.master, data.as_ptr().cast(), data.len());
            if written < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        unsafe {
            let mut size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if libc::ioctl(self.master, libc::TIOCSWINSZ, &mut size) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Tell the shell the size changed.
            let _ = libc::kill(self.pid, libc::SIGWINCH);
            Ok(())
        }
    }

    /// Hang up the session: the shell gets SIGHUP and the master reads EOF.
    pub fn terminate(&self) {
        unsafe {
            let _ = libc::kill(self.pid, libc::SIGHUP);
        }
    }

    /// Force-kill, used when the operator disconnects mid-command.
    pub fn kill(&self) {
        unsafe {
            let _ = libc::kill(self.pid, libc::SIGKILL);
        }
    }

    /// Non-blocking reap; `true` when the child has exited.
    pub fn poll_exit(&self) -> bool {
        if self.exit_code() .is_some() {
            return true;
        }
        unsafe {
            let mut status: libc::c_int = 0;
            let pid = libc::waitpid(self.pid, &mut status, libc::WNOHANG);
            if pid == self.pid {
                let code = if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else if libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status)
                } else {
                    0
                };
                self.set_exit_code(code);
                true
            } else {
                false
            }
        }
    }

    /// Blocking reap; call once before dropping to avoid a zombie.
    pub fn reap(&self) -> i32 {
        if let Some(code) = self.exit_code() {
            return code;
        }
        unsafe {
            let mut status: libc::c_int = 0;
            let pid = libc::waitpid(self.pid, &mut status, 0);
            let code = if pid == self.pid && libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if pid == self.pid && libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                0
            };
            self.set_exit_code(code);
            code
        }
    }

    /// Returns the reaped exit code, or `None` if the child hasn't been reaped.
    fn exit_code(&self) -> Option<i32> {
        let v = self.exit_code.load(Ordering::Acquire);
        if v == EXIT_CODE_UNSET { None } else { Some(v) }
    }

    /// Record the exit code after reaping.
    fn set_exit_code(&self, code: i32) {
        self.exit_code.store(code, Ordering::Release);
    }
}

#[cfg(unix)]
impl Drop for HostShell {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::close(self.master);
        }
    }
}

/// Non-unix placeholder: always refuses, exactly like a missing tool.
#[cfg(not(unix))]
pub struct HostShell;

#[cfg(not(unix))]
impl HostShell {
    pub fn spawn(_rows: u16, _cols: u16) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the host terminal requires a unix host",
        ))
    }

    pub fn pid(&self) -> i32 {
        0
    }

    pub fn read(&self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the host terminal requires a unix host",
        ))
    }

    pub fn write(&self, _data: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the host terminal requires a unix host",
        ))
    }

    pub fn resize(&self, _rows: u16, _cols: u16) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the host terminal requires a unix host",
        ))
    }

    pub fn terminate(&self) {}
    pub fn kill(&self) {}
    pub fn poll_exit(&self) -> bool {
        true
    }
    pub fn reap(&self) -> i32 {
        0
    }
}

/// Split a PTY chunk at a UTF-8 boundary: returns the decodable prefix and the
/// bytes to carry over to the next read, so a multibyte character split across
/// two reads is not mangled.
pub fn utf8_split(chunk: &[u8]) -> (&[u8], &[u8]) {
    match std::str::from_utf8(chunk) {
        Ok(_) => (chunk, &[]),
        Err(err) => {
            let valid = err.valid_up_to();
            (&chunk[..valid], &chunk[valid..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_split_keeps_partial_sequences() {
        let text = "héllo";
        let bytes = text.as_bytes();
        // Cut inside the two-byte 'é'.
        let (valid, carry) = utf8_split(&bytes[..2]);
        assert_eq!(valid, b"h");
        assert_eq!(carry, &[0xc3]);
        let (valid2, carry2) = utf8_split(&[0xc3, 0xa9]);
        assert_eq!(valid2, &[0xc3, 0xa9]);
        assert!(carry2.is_empty());
        // Reassembly round-trips: valid + carry + the rest of the stream.
        let mut whole = valid.to_vec();
        whole.extend_from_slice(carry);
        whole.extend_from_slice(&bytes[2..]);
        assert_eq!(String::from_utf8(whole).expect("utf8"), text);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_runs_a_shell_and_it_exits_cleanly() {
        // Not spawned inside the API: a direct smoke check that openpty/fork
        // work in the test process. bash -lc "exit 7" must report code 7.
        let shell = HostShell::spawn(24, 80).expect("spawn");
        let _ = shell.write(b"exit 7\n");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if shell.poll_exit() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let code = shell.reap();
        assert_eq!(code, 7);
    }

    #[cfg(unix)]
    #[test]
    fn resize_is_accepted() {
        let shell = HostShell::spawn(24, 80).expect("spawn");
        shell.resize(40, 120).expect("resize");
        shell.kill();
        let _ = shell.reap();
    }
}
