//! Owned production children. Dropping a guard always kills and reaps its child.

use crate::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use std::{
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) struct Process {
    child: Mutex<Child>,
    exit_reported: bool,
}

impl Process {
    pub fn spawn(command: &mut Command, log: &Path) -> Result<Self, TrellisTestError> {
        let file = OpenOptions::new().create(true).append(true).open(log)?;
        // Match the Node testkit's per-host budget without overriding user tuning.
        command.env(
            "TOKIO_WORKER_THREADS",
            std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| "2".into()),
        );
        let child = command.stdout(file.try_clone()?).stderr(file).spawn()?;
        Ok(Self {
            child: Mutex::new(child),
            exit_reported: false,
        })
    }

    pub fn running(&self) -> Result<bool, TrellisTestError> {
        Ok(self
            .child
            .lock()
            .expect("process owner poisoned")
            .try_wait()?
            .is_none())
    }

    pub fn check_startup(&mut self) -> Result<(), TrellisTestError> {
        if self.running()? {
            return Ok(());
        }
        self.exit_reported = true;
        Err(TrellisTestError::new(
            TrellisTestErrorKind::Runtime,
            TrellisTestStage::RuntimeStart,
            "production process exited before readiness; inspect the retained private logs",
        ))
    }

    pub fn id(&self) -> u32 {
        self.child.lock().expect("process owner poisoned").id()
    }

    pub async fn stop(&mut self, timeout: Duration) -> Result<(), TrellisTestError> {
        let child = self.child.get_mut().expect("process owner poisoned");
        if child.try_wait()?.is_none() {
            // SAFETY: this is our unreaped child PID; SIGTERM takes no pointers.
            if unsafe { libc::kill(child.id() as i32, libc::SIGTERM) } != 0 {
                let cause = std::io::Error::last_os_error();
                if cause.raw_os_error() != Some(libc::ESRCH) {
                    return Err(cause.into());
                }
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                if status.success() || self.exit_reported {
                    return Ok(());
                }
                self.exit_reported = true;
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::Runtime,
                    TrellisTestStage::Shutdown,
                    format!("production child exited with {status}"),
                ));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                child.wait()?;
                self.exit_reported = true;
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::Timeout,
                    TrellisTestStage::Shutdown,
                    "production child did not stop gracefully before its deadline",
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

pub(crate) async fn run(
    command: &mut Command,
    log: &Path,
    input: Option<&str>,
    stage: TrellisTestStage,
) -> Result<(), TrellisTestError> {
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut process = Process::spawn(command, log)?;
    let child = process.child.get_mut().expect("process owner poisoned");
    if let Some(input) = input {
        writeln!(child.stdin.take().expect("piped stdin"), "{input}")?;
    }
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(TrellisTestError::new(
                    TrellisTestErrorKind::Bootstrap,
                    stage,
                    format!(
                        "production command failed with {status}; inspect the retained private \
                        command log"
                    ),
                ))
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let child = self.child.get_mut().expect("process owner poisoned");
        let _ = child.kill();
        let _ = child.wait();
    }
}
