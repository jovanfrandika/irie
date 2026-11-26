//! Launching `irie player-host` with piped stdin/stdout/stderr.
//!
//! On macOS the helper is spawned as its own "responsible" process, so privacy
//! prompts (audio capture for the visualizer) name irie itself, using the
//! Info.plist embedded in the binary, instead of being silently denied for the
//! terminal app that started irie.

use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

pub type Reader = Box<dyn AsyncRead + Send + Unpin>;
pub type Writer = Box<dyn AsyncWrite + Send + Unpin>;

pub struct HostProc {
    pub stdin: Option<Writer>,
    pub stdout: Option<Reader>,
    pub stderr: Option<Reader>,
    handle: Handle,
}

enum Handle {
    #[cfg(target_os = "macos")]
    Pid(i32),
    #[allow(dead_code)]
    Child(tokio::process::Child),
}

impl HostProc {
    pub fn spawn(exe: &Path, args: &[&str]) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        if let Ok(p) = mac::spawn_disclaimed(exe, args) {
            return Ok(p);
        }
        spawn_plain(exe, args)
    }

    /// Wait up to `grace` for the process to exit, then kill it.
    pub async fn wait_or_kill(&mut self, grace: Duration) {
        match &mut self.handle {
            #[cfg(target_os = "macos")]
            Handle::Pid(pid) => {
                let pid = *pid;
                let deadline = std::time::Instant::now() + grace;
                loop {
                    let mut st = 0;
                    // SAFETY: plain waitpid on our own child.
                    let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
                    if r != 0 {
                        return;
                    }
                    if std::time::Instant::now() >= deadline {
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                            libc::waitpid(pid, &mut st, 0);
                        }
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            Handle::Child(c) => {
                if tokio::time::timeout(grace, c.wait()).await.is_err() {
                    let _ = c.kill().await;
                }
            }
        }
    }
}

impl Drop for HostProc {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Handle::Pid(pid) = self.handle {
            // SAFETY: signalling and reaping our own child.
            unsafe {
                let mut st = 0;
                if libc::waitpid(pid, &mut st, libc::WNOHANG) == 0 {
                    libc::kill(pid, libc::SIGTERM);
                    // Reap in the background so it doesn't linger as a zombie.
                    std::thread::spawn(move || {
                        let mut st = 0;
                        libc::waitpid(pid, &mut st, 0);
                    });
                }
            }
        }
    }
}

fn spawn_plain(exe: &Path, args: &[&str]) -> io::Result<HostProc> {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    Ok(HostProc {
        stdin: child.stdin.take().map(|s| Box::new(s) as Writer),
        stdout: child.stdout.take().map(|s| Box::new(s) as Reader),
        stderr: child.stderr.take().map(|s| Box::new(s) as Reader),
        handle: Handle::Child(child),
    })
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use tokio::net::unix::pipe;

    const POSIX_SPAWN_CLOEXEC_DEFAULT: libc::c_short = 0x4000;

    unsafe extern "C" {
        fn _NSGetEnviron() -> *mut *const *const c_char;
    }

    fn check(r: c_int) -> io::Result<()> {
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(r))
        }
    }

    fn pipe_pair() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0; 2];
        // SAFETY: fds is a valid 2-element buffer.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: freshly created, owned descriptors.
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }

    fn nonblocking(fd: &OwnedFd) {
        use std::os::fd::AsRawFd;
        unsafe {
            let fl = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }

    pub fn spawn_disclaimed(exe: &Path, args: &[&str]) -> io::Result<HostProc> {
        use std::os::fd::AsRawFd;
        // SAFETY: looking up an optional libSystem symbol.
        let disclaim = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_spawnattrs_setdisclaim".as_ptr(),
            )
        };
        if disclaim.is_null() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "no disclaim"));
        }
        let disclaim: unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int =
            unsafe { std::mem::transmute(disclaim) };

        let (in_r, in_w) = pipe_pair()?;
        let (out_r, out_w) = pipe_pair()?;
        let (err_r, err_w) = pipe_pair()?;

        let path = CString::new(exe.as_os_str().as_bytes())?;
        let mut argv_c = vec![path.clone()];
        for a in args {
            argv_c.push(CString::new(*a)?);
        }
        let mut argv: Vec<*mut c_char> = argv_c.iter().map(|s| s.as_ptr() as *mut c_char).collect();
        argv.push(std::ptr::null_mut());

        unsafe {
            let mut fa: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
            let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
            check(libc::posix_spawn_file_actions_init(&mut fa))?;
            check(libc::posix_spawnattr_init(&mut attr))?;
            let cleanup = |fa: &mut libc::posix_spawn_file_actions_t,
                           attr: &mut libc::posix_spawnattr_t| {
                libc::posix_spawn_file_actions_destroy(fa);
                libc::posix_spawnattr_destroy(attr);
            };
            let res = (|| -> io::Result<i32> {
                check(libc::posix_spawn_file_actions_adddup2(
                    &mut fa,
                    in_r.as_raw_fd(),
                    0,
                ))?;
                check(libc::posix_spawn_file_actions_adddup2(
                    &mut fa,
                    out_w.as_raw_fd(),
                    1,
                ))?;
                check(libc::posix_spawn_file_actions_adddup2(
                    &mut fa,
                    err_w.as_raw_fd(),
                    2,
                ))?;
                check(libc::posix_spawnattr_setflags(
                    &mut attr,
                    POSIX_SPAWN_CLOEXEC_DEFAULT,
                ))?;
                check(disclaim(&mut attr, 1))?;
                let mut pid = 0;
                check(libc::posix_spawn(
                    &mut pid,
                    path.as_ptr(),
                    &fa,
                    &attr,
                    argv.as_ptr(),
                    *_NSGetEnviron() as *const *mut c_char,
                ))?;
                Ok(pid)
            })();
            cleanup(&mut fa, &mut attr);
            let pid = res?;
            drop((in_r, out_w, err_w));
            for fd in [&in_w, &out_r, &err_r] {
                nonblocking(fd);
            }
            let _ = std::ptr::null::<c_void>();
            Ok(HostProc {
                stdin: Some(Box::new(pipe::Sender::from_owned_fd(in_w)?)),
                stdout: Some(Box::new(pipe::Receiver::from_owned_fd(out_r)?)),
                stderr: Some(Box::new(pipe::Receiver::from_owned_fd(err_r)?)),
                handle: Handle::Pid(pid),
            })
        }
    }
}
