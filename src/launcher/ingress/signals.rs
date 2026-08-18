//! Unix signal ownership for the pane-side ingress process.

use std::process::Command;

use anyhow::Result;

const INGRESS_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGHUP, libc::SIGTERM];

pub(super) struct IngressSignalGuard {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl IngressSignalGuard {
    // The shell waits for ingress, not its child. Keep ingress alive for signals
    // sent to their shared foreground process group so it remains the sole owner
    // that reaps the launcher before the shell can resume.
    pub(super) fn install() -> Result<Self> {
        let mut guard = Self {
            previous: Vec::with_capacity(INGRESS_SIGNALS.len()),
        };
        for signal in INGRESS_SIGNALS {
            let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
            action.sa_sigaction = retain_ingress_ownership as *const () as usize;
            action.sa_flags = 0;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }

            let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            guard.previous.push((signal, previous));
        }
        Ok(guard)
    }
}

impl Drop for IngressSignalGuard {
    fn drop(&mut self) {
        for (signal, action) in self.previous.iter().rev() {
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
    }
}

extern "C" fn retain_ingress_ownership(_signal: libc::c_int) {}

pub(super) fn configure_child_signal_defaults(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            for signal in INGRESS_SIGNALS {
                let mut action = std::mem::zeroed::<libc::sigaction>();
                action.sa_sigaction = libc::SIG_DFL;
                action.sa_flags = 0;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}
