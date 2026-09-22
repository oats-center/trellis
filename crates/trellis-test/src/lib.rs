//! Private process-lifecycle support for the Rust CLI live test.

use std::process::Command;

/// Configures a test child to receive `SIGTERM` if its parent exits on Linux.
pub fn terminate_on_parent_exit(_command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::{io, os::unix::process::CommandExt as _};

        let parent_pid = std::process::id();
        // SAFETY: `pre_exec` invokes only async-signal-safe libc calls before exec.
        unsafe {
            _command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid.cast_signed() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "test parent exited before child startup",
                    ));
                }
                Ok(())
            });
        }
    }
}
