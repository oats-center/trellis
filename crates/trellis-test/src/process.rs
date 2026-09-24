//! Child-process ownership, bounded output capture, and a supervisor thread.
//!
//! One private supervisor thread per runtime owns every child process group. It
//! is independent of the caller's Tokio executor: async methods only await
//! channel acknowledgements, so operating-system children are never waited on
//! from a Tokio worker. The small unsafe OS boundary (`setpgid`, `prctl`,
//! `kill`) is centralized here.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::sandbox::Sandbox;

/// Maximum bytes retained for a stream's diagnostic tail.
const TAIL_LIMIT: usize = 16 * 1024;
/// Maximum bytes buffered for a single unterminated log line.
const LINE_LIMIT: usize = 64 * 1024;
/// Grace period after `SIGTERM` to the server leader.
const LEADER_TERM_GRACE: Duration = Duration::from_secs(10);
/// Extra grace after `SIGTERM` to the remaining process group.
const GROUP_TERM_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct OutputTailInner {
    bytes: VecDeque<u8>,
    line: Vec<u8>,
}

/// Shared handle to a bounded output tail.
#[derive(Clone, Default)]
pub(crate) struct OutputTail {
    inner: Arc<Mutex<OutputTailInner>>,
}

impl OutputTail {
    fn push(&self, chunk: &[u8]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        for &byte in chunk {
            if inner.bytes.len() == TAIL_LIMIT {
                inner.bytes.pop_front();
            }
            inner.bytes.push_back(byte);
            if byte == b'\n' {
                inner.line.clear();
            } else if inner.line.len() < LINE_LIMIT {
                inner.line.push(byte);
            }
        }
    }

    /// Lossy UTF-8 rendering of the retained tail (split sequences become `\u{FFFD}`).
    pub(crate) fn text(&self) -> String {
        let Ok(inner) = self.inner.lock() else {
            return String::new();
        };
        let bytes: Vec<u8> = inner.bytes.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// A spawned child owned by the supervisor.
struct OwnedChild {
    child: Child,
    pid: u32,
    pgid: i32,
    exited: Option<i32>,
    readers: Vec<JoinHandle<()>>,
}

enum SupervisorCommand {
    Adopt(OwnedChild),
    Status(u32, Sender<Option<i32>>),
    Stop(Sender<Vec<TrellisTestError>>),
}

/// Owns child processes and performs all waits and signals.
pub(crate) struct ProcessSupervisor {
    tx: Sender<SupervisorCommand>,
    thread: Option<JoinHandle<()>>,
}

impl ProcessSupervisor {
    /// Starts the supervisor thread.
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("trellis-test-supervisor".to_owned())
            .spawn(move || supervise(rx))
            .expect("spawn supervisor thread");
        Self {
            tx,
            thread: Some(thread),
        }
    }

    /// Spawns a child in its own process group and adopts it for supervision.
    pub(crate) fn spawn(
        &self,
        command: &mut Command,
        sandbox: &Sandbox,
        path: &OsString,
        role: &'static str,
    ) -> Result<(u32, OutputTail, OutputTail), TrellisTestError> {
        sandbox.apply_child_env(command, path);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        configure_process_group(command);
        let mut child = command.spawn().map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::ProcessExited,
                TrellisTestStage::ServerStart,
                format!("spawning the {role} process: {error}"),
            )
        })?;
        let pgid = child.id() as i32;
        let pid = child.id();
        let stdout = OutputTail::default();
        let stderr = OutputTail::default();
        let mut readers = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            readers.push(read_stream(pipe, stdout.clone()));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(read_stream(pipe, stderr.clone()));
        }
        self.tx
            .send(SupervisorCommand::Adopt(OwnedChild {
                child,
                pid,
                pgid,
                exited: None,
                readers,
            }))
            .map_err(|_| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Cleanup,
                    TrellisTestStage::ServerStart,
                    "the process supervisor is not running",
                )
            })?;
        Ok((pid, stdout, stderr))
    }

    /// Returns the exit code of a spawned child, or `None` while it runs.
    pub(crate) fn exit_code(&self, pid: u32) -> Option<i32> {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(SupervisorCommand::Status(pid, tx)).is_err() {
            return None;
        }
        rx.recv_timeout(Duration::from_secs(2)).ok().flatten()
    }

    /// Terminates every owned child within `deadline` and reports failures.
    pub(crate) fn stop(&self, deadline: Duration) -> Vec<TrellisTestError> {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(SupervisorCommand::Stop(ack_tx)).is_err() {
            return Vec::new();
        }
        match ack_rx.recv_timeout(deadline) {
            Ok(failures) => failures,
            Err(_) => vec![TrellisTestError::new(
                TrellisTestErrorKind::Cleanup,
                TrellisTestStage::Shutdown,
                "the process supervisor did not finish cleanup within the deadline",
            )],
        }
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        let _ = self.tx.send(SupervisorCommand::Stop(ack_tx));
        let _ = ack_rx.recv_timeout(Duration::from_secs(30));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_stream(mut pipe: impl Read + Send + 'static, tail: OutputTail) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => tail.push(&buffer[..count]),
            }
        }
    })
}

fn supervise(rx: Receiver<SupervisorCommand>) {
    let mut children: Vec<OwnedChild> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(SupervisorCommand::Adopt(child)) => children.push(child),
            Ok(SupervisorCommand::Status(pid, ack)) => {
                poll_exits(&mut children);
                let _ = ack.send(
                    children
                        .iter()
                        .find(|child| child.pid == pid)
                        .and_then(|child| child.exited),
                );
            }
            Ok(SupervisorCommand::Stop(ack)) => {
                let _ = ack.send(terminate_all(&mut children));
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => poll_exits(&mut children),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = terminate_all(&mut children);
}

fn poll_exits(children: &mut [OwnedChild]) {
    for child in children.iter_mut() {
        if child.exited.is_some() {
            continue;
        }
        if let Ok(Some(status)) = child.child.try_wait() {
            child.exited = Some(status.code().unwrap_or(-1));
        }
    }
}

fn terminate_all(children: &mut Vec<OwnedChild>) -> Vec<TrellisTestError> {
    for mut owned in children.drain(..) {
        terminate_child(&mut owned);
    }
    Vec::new()
}

fn terminate_child(owned: &mut OwnedChild) {
    #[cfg(unix)]
    {
        // Ask the server leader to stop; it shuts down its managed NATS child.
        let _ = signal_group(owned.pgid, libc::SIGTERM);
        if !wait_leader_exit(owned, LEADER_TERM_GRACE) {
            // The leader ignored SIGTERM: ask the remaining group, then force.
            let _ = signal_group(owned.pgid, libc::SIGTERM);
            std::thread::sleep(GROUP_TERM_GRACE.min(Duration::from_millis(200)));
            let _ = signal_group(owned.pgid, libc::SIGKILL);
        }
    }
    let _ = owned.child.wait();
    for reader in owned.readers.drain(..) {
        let _ = reader.join();
    }
}

fn wait_leader_exit(owned: &mut OwnedChild, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        match owned.child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn signal_group(pgid: i32, signal: i32) -> std::io::Result<()> {
    // SAFETY: a negative pid targets the process group; `ESRCH` is not an error
    // for cleanup.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: only async-signal-safe calls (`setpgid`, `prctl`, `getppid`) run
    // between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                let parent = libc::getppid();
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent || parent == 1 {
                    return Err(std::io::Error::other("parent exited before exec"));
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

/// Runs a short-lived command, capturing bounded output, with a deadline.
///
/// Returns `(exit_success, stdout, stderr)`.
pub(crate) fn run_captured(
    command: &mut Command,
    sandbox: &Sandbox,
    path: &OsString,
    deadline: Duration,
) -> Result<(bool, String, String), TrellisTestError> {
    sandbox.apply_child_env(command, path);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    configure_process_group(command);
    let mut child = command.spawn().map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::ProcessExited,
            TrellisTestStage::VersionCheck,
            format!("spawning a helper command: {error}"),
        )
    })?;
    let stdout = OutputTail::default();
    let stderr = OutputTail::default();
    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        readers.push(read_stream(pipe, stdout.clone()));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.push(read_stream(pipe, stderr.clone()));
    }
    let deadline_at = Instant::now() + deadline;
    let mut success = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                success = status.success();
                break;
            }
            Ok(None) => {}
            Err(error) => {
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::Io,
                    TrellisTestStage::VersionCheck,
                    format!("waiting for a helper command: {error}"),
                ));
            }
        }
        if Instant::now() >= deadline_at {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::VersionCheck,
                "a helper command exceeded its deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for reader in readers.drain(..) {
        let _ = reader.join();
    }
    Ok((success, stdout.text(), stderr.text()))
}
