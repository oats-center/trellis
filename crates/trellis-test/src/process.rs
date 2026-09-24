//! Child-process ownership, bounded output capture, and a supervisor thread.
//!
//! One private supervisor thread per runtime owns every child process group. It
//! is independent of the caller's Tokio executor: async methods only await
//! channel acknowledgements, so operating-system children are never waited on
//! from a Tokio worker. The small unsafe OS boundary (`setpgid`, `prctl`,
//! `waitpid`, `kill`) is centralized here.
//!
//! Exit observation uses `waitpid(..., WNOWAIT)`: the supervisor learns that a
//! child exited without reaping it, so the leader's identity and process-group
//! id remain owned and valid until final group cleanup. Only the final reap
//! discards that identity.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

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
/// Supervisor poll interval for exit observation and command handling.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Default)]
struct OutputTailInner {
    /// Private raw bytes retained for parsing.
    bytes: VecDeque<u8>,
    /// Private partial line retained for parsing.
    line: Vec<u8>,
    /// Public bytes, redacted as they were captured.
    sanitized: VecDeque<u8>,
    /// Redacted bytes held back so a secret split across chunks is still caught.
    carry: Vec<u8>,
    /// Known secret byte strings.
    secrets: Vec<Vec<u8>>,
    /// Longest registered secret length.
    max_secret: usize,
}

/// Shared handle to a bounded output tail.
///
/// Raw bytes stay private for parsing, and [`OutputTail::text`] renders a copy
/// that is redacted as it is captured. Because redaction happens before the
/// tail is truncated, a secret split by the tail boundary cannot leave a
/// recognizable suffix behind.
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
        let mut combined = std::mem::take(&mut inner.carry);
        combined.extend_from_slice(chunk);
        let redacted = redact_bytes(&combined, &inner.secrets);
        inner.split_sanitized(redacted);
    }

    /// Registers a secret to redact from the public diagnostic tail.
    ///
    /// Retained bytes are re-redacted, so a secret discovered after capture (for
    /// example a bootstrap token) does not linger in already-captured output.
    pub(crate) fn add_secret(&self, secret: &[u8]) {
        if secret.is_empty() {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.secrets.iter().any(|existing| existing == secret) {
            return;
        }
        inner.secrets.push(secret.to_vec());
        inner.max_secret = inner.max_secret.max(secret.len());
        let mut combined: Vec<u8> = inner.sanitized.iter().copied().collect();
        combined.extend_from_slice(&inner.carry);
        inner.sanitized.clear();
        let redacted = redact_bytes(&combined, &inner.secrets);
        inner.split_sanitized(redacted);
    }

    /// Lossy UTF-8 rendering of the redacted tail (split sequences become `\u{FFFD}`).
    pub(crate) fn text(&self) -> String {
        let Ok(inner) = self.inner.lock() else {
            return String::new();
        };
        let mut bytes: Vec<u8> = inner.sanitized.iter().copied().collect();
        bytes.extend_from_slice(&inner.carry);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl OutputTailInner {
    fn split_sanitized(&mut self, redacted: Vec<u8>) {
        let carry_len = self.max_secret.saturating_sub(1);
        if redacted.len() > carry_len {
            let split = redacted.len() - carry_len;
            for &byte in &redacted[..split] {
                if self.sanitized.len() == TAIL_LIMIT {
                    self.sanitized.pop_front();
                }
                self.sanitized.push_back(byte);
            }
            self.carry = redacted[split..].to_vec();
        } else {
            self.carry = redacted;
        }
    }
}

/// Replaces every non-overlapping occurrence of each secret.
fn redact_bytes(haystack: &[u8], secrets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = haystack.to_vec();
    for secret in secrets {
        out = replace_bytes(&out, secret, b"[REDACTED]");
    }
    out
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].starts_with(needle) {
            out.extend_from_slice(replacement);
            index += needle.len();
        } else {
            out.push(haystack[index]);
            index += 1;
        }
    }
    out
}

/// A child spawned in its own process group and owned by the supervisor.
struct OwnedChild {
    child: Child,
    pid: u32,
    pgid: i32,
    role: &'static str,
    /// Observed exit status, set without reaping until [`reap_leader`].
    exited: Option<i32>,
    readers: Vec<JoinHandle<()>>,
    reader_done: Receiver<Result<(), String>>,
    reader_count: usize,
}

enum SupervisorCommand {
    Adopt(OwnedChild),
    Status {
        pid: u32,
        ack: oneshot::Sender<Option<i32>>,
    },
    WaitCaptured {
        pid: u32,
        deadline: Instant,
        ack: oneshot::Sender<Result<bool, TrellisTestError>>,
    },
    Stop {
        deadline: Instant,
        ack: oneshot::Sender<Vec<TrellisTestError>>,
    },
    Shutdown {
        deadline: Instant,
        ack: oneshot::Sender<Vec<TrellisTestError>>,
    },
}

/// Callback invoked with each complete line of a child's output as it arrives.
///
/// Used to capture bootstrap output durably before the rolling tail can discard it.
pub(crate) type LineObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// Handle to a spawned child returned to the caller.
pub(crate) struct SpawnedChild {
    /// Operating-system process id.
    pub(crate) pid: u32,
    /// Bounded stdout tail.
    pub(crate) stdout: OutputTail,
    /// Bounded stderr tail.
    pub(crate) stderr: OutputTail,
}

/// Owns child processes and performs all waits and signals on its own thread.
pub(crate) struct ProcessSupervisor {
    tx: Sender<SupervisorCommand>,
    thread: Option<JoinHandle<()>>,
    drop_deadline: Duration,
}

impl ProcessSupervisor {
    /// Starts the supervisor thread with the deadline used for best-effort drop cleanup.
    pub(crate) fn new(shutdown_deadline: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let deadline = if shutdown_deadline.is_zero() {
            Duration::from_secs(30)
        } else {
            shutdown_deadline
        };
        let thread = std::thread::Builder::new()
            .name("trellis-test-supervisor".to_owned())
            .spawn(move || supervise(rx, deadline))
            .expect("spawn supervisor thread");
        Self {
            tx,
            thread: Some(thread),
            drop_deadline: deadline,
        }
    }

    /// Spawns a child in its own process group and adopts it for supervision.
    pub(crate) fn spawn(
        &self,
        command: &mut Command,
        sandbox: &Sandbox,
        path: &OsString,
        role: &'static str,
        observer: Option<LineObserver>,
    ) -> Result<SpawnedChild, TrellisTestError> {
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
        let pid = child.id();
        let pgid = pid as i32;
        let stdout = OutputTail::default();
        let stderr = OutputTail::default();
        let (done_tx, reader_done) = mpsc::channel();
        let mut readers = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            readers.push(read_stream(
                pipe,
                stdout.clone(),
                done_tx.clone(),
                observer.clone(),
            ));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(read_stream(
                pipe,
                stderr.clone(),
                done_tx.clone(),
                observer.clone(),
            ));
        }
        let reader_count = readers.len();
        drop(done_tx);
        self.tx
            .send(SupervisorCommand::Adopt(OwnedChild {
                child,
                pid,
                pgid,
                role,
                exited: None,
                readers,
                reader_done,
                reader_count,
            }))
            .map_err(|_| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Cleanup,
                    TrellisTestStage::ServerStart,
                    "the process supervisor is not running",
                )
            })?;
        Ok(SpawnedChild {
            pid,
            stdout,
            stderr,
        })
    }

    /// Returns the exit code of a spawned child, or `None` while it runs.
    ///
    /// Observation does not reap the child, so the leader identity remains owned.
    pub(crate) async fn exit_code(&self, pid: u32) -> Option<i32> {
        let (ack, rx) = oneshot::channel();
        if self
            .tx
            .send(SupervisorCommand::Status { pid, ack })
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Waits for a transient child to exit within `deadline`, terminating its
    /// group and reporting forced cleanup if it overruns.
    pub(crate) async fn wait_captured(
        &self,
        pid: u32,
        deadline: Instant,
    ) -> Result<bool, TrellisTestError> {
        let (ack, rx) = oneshot::channel();
        if self
            .tx
            .send(SupervisorCommand::WaitCaptured { pid, deadline, ack })
            .is_err()
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Cleanup,
                TrellisTestStage::VersionCheck,
                "the process supervisor is not running",
            ));
        }
        match tokio::time::timeout_at(deadline.into(), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(TrellisTestError::new(
                TrellisTestErrorKind::ProcessExited,
                TrellisTestStage::VersionCheck,
                "the process supervisor exited before reporting a helper result",
            )),
            Err(_) => Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::VersionCheck,
                "a helper command exceeded its deadline",
            )),
        }
    }

    /// Terminates every owned child within `deadline` and reports failures.
    /// Stops current children and reports whether the supervisor finished
    /// cleanup before the deadline.
    pub(crate) async fn stop(&self, deadline: Instant) -> (Vec<TrellisTestError>, bool) {
        let (ack, rx) = oneshot::channel();
        if self
            .tx
            .send(SupervisorCommand::Stop { deadline, ack })
            .is_err()
        {
            return (Vec::new(), false);
        }
        match tokio::time::timeout_at(deadline.into(), rx).await {
            Ok(Ok(failures)) => (failures, false),
            Ok(Err(_)) => (
                vec![TrellisTestError::new(
                    TrellisTestErrorKind::Cleanup,
                    TrellisTestStage::Shutdown,
                    "the process supervisor exited before reporting cleanup",
                )],
                false,
            ),
            Err(_) => (
                vec![TrellisTestError::new(
                    TrellisTestErrorKind::Cleanup,
                    TrellisTestStage::Shutdown,
                    "the process supervisor did not finish cleanup within the deadline",
                )],
                true,
            ),
        }
    }

    /// Sends a best-effort stop request without waiting for completion.
    pub(crate) fn request_stop(&self) {
        let (ack, _rx) = oneshot::channel();
        let deadline = Instant::now() + self.drop_deadline;
        let _ = self.tx.send(SupervisorCommand::Stop { deadline, ack });
    }

    /// Signals the supervisor to finish cleanup and blocks until its thread ends.
    ///
    /// Called only from a dedicated cleanup thread, never from an async task or
    /// from `Drop` on the caller's thread.
    pub(crate) fn join(mut self) {
        let (ack, _rx) = oneshot::channel();
        let deadline = Instant::now() + self.drop_deadline;
        let _ = self.tx.send(SupervisorCommand::Shutdown { deadline, ack });
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        // Best effort: hand cleanup to the supervisor thread without blocking.
        self.request_stop();
    }
}

fn read_stream(
    mut pipe: impl Read + Send + 'static,
    tail: OutputTail,
    done: mpsc::Sender<Result<(), String>>,
    observer: Option<LineObserver>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        let mut line: Vec<u8> = Vec::new();
        let outcome = loop {
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    if let Some(observer) = &observer {
                        if !line.is_empty() {
                            observer(&String::from_utf8_lossy(&line));
                        }
                    }
                    break Ok(());
                }
                Ok(count) => {
                    tail.push(&buffer[..count]);
                    if let Some(observer) = &observer {
                        for &byte in &buffer[..count] {
                            if byte == b'\n' || line.len() >= LINE_LIMIT {
                                observer(&String::from_utf8_lossy(&line));
                                line.clear();
                            } else {
                                line.push(byte);
                            }
                        }
                    }
                }
                Err(error) => break Err(error.to_string()),
            }
        };
        let _ = done.send(outcome);
    })
}

fn supervise(rx: Receiver<SupervisorCommand>, drop_deadline: Duration) {
    let mut children: Vec<OwnedChild> = Vec::new();
    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(SupervisorCommand::Adopt(child)) => children.push(child),
            Ok(SupervisorCommand::Status { pid, ack }) => {
                peek_exits(&mut children);
                let code = children
                    .iter()
                    .find(|child| child.pid == pid)
                    .and_then(|child| child.exited);
                let _ = ack.send(code);
            }
            Ok(SupervisorCommand::WaitCaptured { pid, deadline, ack }) => {
                let result = wait_captured_child(&mut children, pid, deadline);
                let _ = ack.send(result);
            }
            Ok(SupervisorCommand::Stop { deadline, ack }) => {
                // Terminate current children but keep supervising: a startup
                // attempt may be retired before a retry reuses this supervisor.
                let failures = terminate_all(&mut children, deadline);
                let _ = ack.send(failures);
            }
            Ok(SupervisorCommand::Shutdown { deadline, ack }) => {
                let failures = terminate_all(&mut children, deadline);
                let _ = ack.send(failures);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => peek_exits(&mut children),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let deadline = Instant::now() + drop_deadline;
    let _ = terminate_all(&mut children, deadline);
}

fn wait_captured_child(
    children: &mut Vec<OwnedChild>,
    pid: u32,
    deadline: Instant,
) -> Result<bool, TrellisTestError> {
    let Some(index) = children.iter().position(|child| child.pid == pid) else {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::ProcessExited,
            TrellisTestStage::VersionCheck,
            "the helper process is no longer supervised",
        ));
    };
    loop {
        peek_exits(children);
        if let Some(code) = children[index].exited {
            let mut owned = children.swap_remove(index);
            let mut failures = Vec::new();
            reap_leader(&mut owned, &mut failures);
            join_readers_bounded(&mut owned, deadline, &mut failures);
            return Ok(code == 0);
        }
        if Instant::now() >= deadline {
            let mut owned = children.swap_remove(index);
            let failures = force_kill(&mut owned, deadline);
            let message = if failures.is_empty() {
                "a helper command exceeded its deadline".to_owned()
            } else {
                format!(
                    "a helper command exceeded its deadline; cleanup: {}",
                    render_failures(&failures)
                )
            };
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::VersionCheck,
                message,
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn peek_exits(children: &mut [OwnedChild]) {
    for child in children.iter_mut() {
        if child.exited.is_some() {
            continue;
        }
        if let Some(code) = peek_exit(child) {
            child.exited = Some(code);
        }
    }
}

/// Observes a child exit without reaping it.
#[cfg(unix)]
fn peek_exit(child: &mut OwnedChild) -> Option<i32> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `waitid` with `WNOWAIT` reports an exited child while leaving it
    // waitable, so its identity and process-group id stay owned until final
    // cleanup. Only this supervisor waits on its own children.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return None;
    }
    sigchld_exit_code(&info)
}

/// Decodes the exit code from a `sigchld` `siginfo_t`.
#[cfg(target_os = "linux")]
fn sigchld_exit_code(info: &libc::siginfo_t) -> Option<i32> {
    // Linux's public `siginfo_t` exposes no accessor for the `sigchld` member,
    // so read its documented 64-bit prefix layout directly: three header ints,
    // one padding int, then `pid`, `uid`, and `status`.
    #[repr(C)]
    struct Sigchld {
        _signo: libc::c_int,
        _errno: libc::c_int,
        code: libc::c_int,
        _pad: libc::c_int,
        pid: libc::pid_t,
        _uid: libc::uid_t,
        status: libc::c_int,
    }
    // SAFETY: for a child exit the `sigchld` member is active, and `sigchld`
    // begins at the documented union offset.
    let raw = unsafe { &*(info as *const libc::siginfo_t).cast::<Sigchld>() };
    if raw.pid == 0 {
        return None;
    }
    Some(decode_sigchld(raw.code, raw.status))
}

/// Decodes the exit code from a `sigchld` `siginfo_t` on macOS.
#[cfg(target_os = "macos")]
fn sigchld_exit_code(info: &libc::siginfo_t) -> Option<i32> {
    if info.si_pid == 0 {
        return None;
    }
    Some(decode_sigchld(info.si_code, info.si_status))
}

/// `si_code == CLD_EXITED` carries an exit status; otherwise the signal.
#[cfg(unix)]
fn decode_sigchld(code: libc::c_int, status: libc::c_int) -> i32 {
    const CLD_EXITED: libc::c_int = 1;
    if code == CLD_EXITED {
        status
    } else {
        -status
    }
}

#[cfg(not(unix))]
fn peek_exit(child: &mut OwnedChild) -> Option<i32> {
    match child.child.try_wait() {
        Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
        _ => None,
    }
}

fn terminate_all(children: &mut Vec<OwnedChild>, deadline: Instant) -> Vec<TrellisTestError> {
    let mut failures = Vec::new();
    for mut owned in children.drain(..) {
        terminate_child(&mut owned, deadline, &mut failures);
    }
    failures
}

/// Graceful shutdown: leader first, then the remaining owned group, then force.
fn terminate_child(
    owned: &mut OwnedChild,
    deadline: Instant,
    failures: &mut Vec<TrellisTestError>,
) {
    #[cfg(unix)]
    {
        if owned.exited.is_none() {
            owned.exited = peek_exit(owned);
        }
        if owned.exited.is_none() {
            // Ask the server leader to stop so it can shut down its managed NATS child.
            if let Err(error) = signal_pid(owned.pid, libc::SIGTERM) {
                if !is_esrch(&error) {
                    failures.push(signal_failure(owned.role, "SIGTERM to the leader", &error));
                }
            }
            let leader_deadline = deadline.min(Instant::now() + LEADER_TERM_GRACE);
            let _ = wait_until_exit(owned, leader_deadline);
        }
        // Signal the remaining owned group independently of leader exit.
        if let Err(error) = signal_group(owned.pgid, libc::SIGTERM) {
            if !is_esrch(&error) {
                failures.push(signal_failure(owned.role, "SIGTERM to the group", &error));
            }
        }
        let group_deadline = deadline.min(Instant::now() + GROUP_TERM_GRACE);
        let _ = wait_until_exit(owned, group_deadline);
        let leader_may_be_alive = owned.exited.is_none();
        // A zombie leader keeps the group id signallable, so live group members
        // cannot be probed after the leader exits. Escalate unconditionally while
        // the leader identity is still owned; this is harmless once empty.
        if let Err(error) = signal_group(owned.pgid, libc::SIGKILL) {
            if !is_esrch(&error) {
                failures.push(signal_failure(owned.role, "SIGKILL to the group", &error));
            }
        }
        if leader_may_be_alive {
            failures.push(TrellisTestError::new(
                TrellisTestErrorKind::Cleanup,
                TrellisTestStage::Shutdown,
                format!(
                    "the {} process ignored SIGTERM and was force-stopped",
                    owned.role
                ),
            ));
        }
    }
    reap_leader(owned, failures);
    join_readers_bounded(owned, deadline, failures);
}

/// Forces cleanup of an overrunning transient child.
fn force_kill(owned: &mut OwnedChild, deadline: Instant) -> Vec<TrellisTestError> {
    let mut failures = Vec::new();
    #[cfg(unix)]
    {
        if let Err(error) = signal_group(owned.pgid, libc::SIGKILL) {
            if !is_esrch(&error) {
                failures.push(signal_failure(owned.role, "SIGKILL to the group", &error));
            }
        }
    }
    reap_leader(owned, &mut failures);
    join_readers_bounded(owned, deadline, &mut failures);
    failures
}

fn reap_leader(owned: &mut OwnedChild, failures: &mut Vec<TrellisTestError>) {
    match owned.child.wait() {
        Ok(status) => {
            owned.exited = Some(status.code().unwrap_or(-1));
        }
        Err(error) => failures.push(TrellisTestError::new(
            TrellisTestErrorKind::Cleanup,
            TrellisTestStage::Shutdown,
            format!("reaping the {} process: {error}", owned.role),
        )),
    }
}

/// Waits for the leader to exit by `deadline` without reaping it.
fn wait_until_exit(owned: &mut OwnedChild, deadline: Instant) -> bool {
    loop {
        if owned.exited.is_none() {
            owned.exited = peek_exit(owned);
        }
        if owned.exited.is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Waits for log readers to finish within `deadline`; readers that cannot finish
/// are detached and reported rather than joined without bound.
fn join_readers_bounded(
    owned: &mut OwnedChild,
    deadline: Instant,
    failures: &mut Vec<TrellisTestError>,
) {
    let mut complete = true;
    for _ in 0..owned.reader_count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match owned.reader_done.recv_timeout(remaining) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => failures.push(TrellisTestError::new(
                TrellisTestErrorKind::Io,
                TrellisTestStage::Shutdown,
                format!("capturing {} output: {message}", owned.role),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                failures.push(TrellisTestError::new(
                    TrellisTestErrorKind::Timeout,
                    TrellisTestStage::Shutdown,
                    format!(
                        "the {} log readers did not finish within the deadline",
                        owned.role
                    ),
                ));
                complete = false;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                failures.push(TrellisTestError::new(
                    TrellisTestErrorKind::Io,
                    TrellisTestStage::Shutdown,
                    format!(
                        "the {} log readers stopped without reporting completion",
                        owned.role
                    ),
                ));
                complete = false;
                break;
            }
        }
    }
    if complete {
        for reader in owned.readers.drain(..) {
            let _ = reader.join();
        }
    } else {
        owned.readers.clear();
    }
}

fn render_failures(failures: &[TrellisTestError]) -> String {
    failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(unix)]
fn signal_failure(role: &str, action: &str, error: &std::io::Error) -> TrellisTestError {
    TrellisTestError::new(
        TrellisTestErrorKind::Cleanup,
        TrellisTestStage::Shutdown,
        format!("{action} for the {role} process: {error}"),
    )
}

#[cfg(not(unix))]
fn signal_failure(role: &str, action: &str, error: &std::io::Error) -> TrellisTestError {
    TrellisTestError::new(
        TrellisTestErrorKind::Cleanup,
        TrellisTestStage::Shutdown,
        format!("{action} for the {role} process: {error}"),
    )
}

#[cfg(unix)]
fn is_esrch(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn is_esrch(_error: &std::io::Error) -> bool {
    false
}

#[cfg(unix)]
fn signal_pid(pid: u32, signal: i32) -> std::io::Result<()> {
    // SAFETY: a positive pid targets a single process owned by this supervisor.
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn signal_pid(_pid: u32, _signal: i32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn signal_group(pgid: i32, signal: i32) -> std::io::Result<()> {
    // SAFETY: a negative pid targets the owned process group; `ESRCH` means it
    // is already gone and is not an error for cleanup.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: i32, _signal: i32) -> std::io::Result<()> {
    Ok(())
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

/// Runs a short-lived command through the supervisor, capturing bounded output
/// under an absolute deadline.
///
/// Returns `(exit_success, stdout, stderr)`. The child is owned by the
/// supervisor for its whole lifetime; a timeout force-stops its process group.
pub(crate) async fn run_captured(
    supervisor: &ProcessSupervisor,
    command: &mut Command,
    sandbox: &Sandbox,
    path: &OsString,
    deadline: Instant,
) -> Result<(bool, String, String), TrellisTestError> {
    let spawned = supervisor.spawn(command, sandbox, path, "helper", None)?;
    let success = supervisor.wait_captured(spawned.pid, deadline).await?;
    Ok((success, spawned.stdout.text(), spawned.stderr.text()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_tail_is_bounded() {
        let tail = OutputTail::default();
        tail.push(&vec![b'a'; TAIL_LIMIT + 512]);
        assert_eq!(tail.text().len(), TAIL_LIMIT);
    }

    #[test]
    fn output_tail_handles_split_utf8() {
        let tail = OutputTail::default();
        // An emoji split across two read chunks must still decode.
        tail.push(&[0xF0, 0x9F]);
        tail.push(&[0x98, 0x80]);
        assert!(tail.text().contains('\u{1F600}'));
    }

    #[test]
    fn output_tail_caps_unterminated_lines() {
        let tail = OutputTail::default();
        // A single missing newline must not grow the retained buffer past the limit.
        tail.push(&vec![b'x'; LINE_LIMIT + 4096]);
        assert_eq!(tail.text().len(), TAIL_LIMIT);
    }

    #[test]
    fn line_observer_sees_lines_split_across_reads() {
        struct OneByte(std::vec::IntoIter<u8>);
        impl std::io::Read for OneByte {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                match self.0.next() {
                    Some(byte) if !buffer.is_empty() => {
                        buffer[0] = byte;
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            }
        }

        let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let observer: LineObserver = Arc::new(move |line: &str| {
            if let Ok(mut lines) = sink.lock() {
                lines.push(line.to_owned());
            }
        });
        let (done_tx, done_rx) = mpsc::channel();
        let data =
            b"TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:1/x?adminAccountToken=abc\nnext\n"
                .to_vec();
        let handle = read_stream(
            OneByte(data.into_iter()),
            OutputTail::default(),
            done_tx,
            Some(observer),
        );
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
        handle.join().expect("reader thread");

        let lines = observed.lock().expect("observed lines");
        assert_eq!(
            lines[0], "TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:1/x?adminAccountToken=abc",
            "a bootstrap line split across reads must be reassembled before parsing"
        );
        assert_eq!(lines[1], "next");
    }

    #[test]
    fn diagnostic_tail_redacts_a_secret_split_by_the_tail_boundary() {
        let tail = OutputTail::default();
        let secret: &[u8] = b"0123456789abcdefghijklmnopqrstuv";
        tail.add_secret(secret);
        // Place the secret so the 16 KiB retention boundary would slice it and
        // leave only its suffix in the tail.
        tail.push(secret);
        tail.push(&vec![b'x'; TAIL_LIMIT - 16]);
        let text = tail.text();
        assert!(!text.contains("mnopqrstuv"), "secret suffix leaked: {text}");
        assert!(text.contains("[REDACTED]"));
    }
}
