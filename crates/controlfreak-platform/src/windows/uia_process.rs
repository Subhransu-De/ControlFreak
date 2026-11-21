//! The UI Automation helper process: a warm child process that answers one JSON request line at
//! a time.
//!
//! This follows the OCR helper in `ocr.rs` (request IDs, bounded reply lines, timeouts,
//! retirement) as a separate copy, so that OCR is unchanged by the accessibility work; the two can
//! share one implementation later. A native fault or a call that never returns in the helper cannot
//! take down or hang the server. Starting a process and loading UI Automation costs tens of
//! milliseconds, so one helper is kept warm between requests. A helper serves one request at a time; closing its stdin cancels the request in
//! progress and ends it. A helper is retired after an error, timeout, cancellation, a reply that
//! asks for it, or its kind's request limit, and exits by itself when idle.

// The command type is named for OCR, its first helper; it starts any helper mode.
use crate::OcrHelperCommand as HelperCommand;
use controlfreak_core::{MutationControl, PlatformError};
use serde::de::DeserializeOwned;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const STDERR_TAIL_BYTES: usize = 4 * 1024;
const HELPER_EXIT_GRACE: Duration = Duration::from_secs(1);
const MAX_SKIPPED_LINES: usize = 16;

/// What differs between helper kinds: names in errors, threads and timings, and limits.
pub(super) struct HelperKind {
    /// Names the helper in error messages, for example `OCR`.
    pub label: &'static str,
    /// The operation a cancellation is reported for.
    pub operation: &'static str,
    pub stdout_thread: &'static str,
    pub stderr_thread: &'static str,
    pub spawn_stage: &'static str,
    pub wait_stage: &'static str,
    /// Longest reply line accepted; a larger one means a faulty helper.
    pub reply_limit: u64,
    /// Requests one helper process serves before it is retired.
    pub max_requests: u32,
    pub error: fn(String) -> PlatformError,
    /// Whether a request may be sent again to a fresh helper when a reused one produced nothing
    /// for it. Only a request that changes nothing may: a helper that exited without a reply may
    /// have acted before it exited.
    pub retry_unstarted: bool,
}

/// A reply line that names the request it answers.
pub(super) trait HelperReply: DeserializeOwned {
    fn id(&self) -> u64;
    /// Whether the helper may serve another request after this reply.
    fn keeps_helper(&self) -> bool;
}

/// The warm helper of one kind and its request numbering.
#[derive(Default)]
pub(super) struct HelperPool {
    pub(super) idle: Mutex<Option<WarmHelper>>,
    next_id: AtomicU64,
}

impl HelperPool {
    /// Sends one request, built by `line` from its request ID, to the warm helper or a new one,
    /// and returns the matching reply. Only a helper whose reply keeps it is reused. `sending`
    /// runs immediately before the request line is written to a helper, so a caller can record
    /// that the helper may act from then on.
    ///
    /// A reused helper can exit while idle just as a request arrives. A kind that only reads the
    /// desktop (`retry_unstarted`) then retries once on a fresh helper, which cannot repeat a
    /// desktop effect; any other kind reports the failure.
    pub(super) fn request<R: HelperReply>(
        &self,
        kind: &'static HelperKind,
        command: &HelperCommand,
        control: &MutationControl,
        timeout: Duration,
        line: impl Fn(u64) -> Result<Vec<u8>, PlatformError>,
        sending: &dyn Fn(),
    ) -> Result<R, PlatformError> {
        control.check(kind.operation)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = line(id)?;
        // A concurrent request finds the slot empty and starts its own helper.
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let (mut helper, reused) = match idle.and_then(WarmHelper::if_running) {
            Some(helper) => (helper, true),
            None => (WarmHelper::spawn(kind, command)?, false),
        };
        let reply = match helper.request::<R>(kind, id, &line, control, timeout, sending) {
            Ok(reply) => reply,
            Err(failure)
                if kind.retry_unstarted
                    && reused
                    && !failure.started
                    && !control.is_cancelled() =>
            {
                drop(helper);
                helper = WarmHelper::spawn(kind, command)?;
                helper
                    .request::<R>(kind, id, &line, control, timeout, sending)
                    .map_err(|failure| failure.error)?
            }
            Err(failure) => return Err(failure.error),
        };
        helper.served += 1;
        if reply.keeps_helper() && helper.served < kind.max_requests {
            let mut slot = self
                .idle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(helper);
            }
        }
        Ok(reply)
    }
}

/// A failed request. `started` is false only when the helper produced nothing at all for it, so
/// it is unknown whether work began; a retry is then safe for a read-only request.
struct RequestFailure {
    error: PlatformError,
    started: bool,
}

enum ReplyLine {
    Line(Vec<u8>),
    Eof,
    Failed(String),
}

/// A running helper process with readers for its output.
pub(super) struct WarmHelper {
    kind: &'static HelperKind,
    child: Child,
    stdin: Option<ChildStdin>,
    replies: mpsc::Receiver<ReplyLine>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    served: u32,
}

impl WarmHelper {
    fn spawn(kind: &'static HelperKind, command: &HelperCommand) -> Result<Self, PlatformError> {
        use std::{os::windows::process::CommandExt, process::Stdio};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _timer = controlfreak_core::timing::start(kind.spawn_stage);
        let mut child = std::process::Command::new(command.program())
            .args(command.arguments())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| {
                (kind.error)(format!(
                    "could not start the isolated {} process: {error}",
                    kind.label
                ))
            })?;
        match Self::attach(kind, &mut child) {
            Ok((stdin, replies, stderr_tail)) => Ok(Self {
                kind,
                child,
                stdin: Some(stdin),
                replies,
                stderr_tail,
                served: 0,
            }),
            Err(reason) => {
                let cleanup = terminate(&mut child);
                Err((kind.error)(format!("{reason}{cleanup}")))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn attach(
        kind: &'static HelperKind,
        child: &mut Child,
    ) -> Result<
        (
            ChildStdin,
            mpsc::Receiver<ReplyLine>,
            Arc<Mutex<VecDeque<u8>>>,
        ),
        String,
    > {
        let label = kind.label;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("the isolated {label} process did not expose stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("the isolated {label} process did not expose stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("the isolated {label} process did not expose stderr"))?;
        let (sender, replies) = mpsc::channel();
        let reply_limit = kind.reply_limit;
        thread::Builder::new()
            .name(kind.stdout_thread.to_owned())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let line = match read_bounded_line(&mut stdout, reply_limit) {
                        Ok(Some(line)) => ReplyLine::Line(line),
                        Ok(None) => ReplyLine::Eof,
                        Err(error) => ReplyLine::Failed(error.to_string()),
                    };
                    let last = !matches!(line, ReplyLine::Line(_));
                    if sender.send(line).is_err() || last {
                        // Keep draining so the helper never blocks on a full pipe.
                        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
                        return;
                    }
                }
            })
            .map_err(|error| format!("could not start the {label} stdout reader: {error}"))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        let tail = Arc::clone(&stderr_tail);
        thread::Builder::new()
            .name(kind.stderr_thread.to_owned())
            .spawn(move || {
                let mut stderr = stderr;
                let mut buffer = [0_u8; 1024];
                while let Ok(read) = stderr.read(&mut buffer) {
                    if read == 0 {
                        return;
                    }
                    let mut tail = tail
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    tail.extend(&buffer[..read]);
                    let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                    tail.drain(..excess);
                }
            })
            .map_err(|error| format!("could not start the {label} stderr reader: {error}"))?;
        Ok((stdin, replies, stderr_tail))
    }

    fn is_running(&mut self) -> bool {
        self.stdin.is_some() && matches!(self.child.try_wait(), Ok(None))
    }

    fn if_running(mut self) -> Option<Self> {
        self.is_running().then_some(self)
    }

    fn request<R: HelperReply>(
        &mut self,
        kind: &'static HelperKind,
        id: u64,
        line: &[u8],
        control: &MutationControl,
        timeout: Duration,
        sending: &dyn Fn(),
    ) -> Result<R, RequestFailure> {
        // A warm helper serves every kind of its pool; errors name the request's own kind.
        self.kind = kind;
        let label = kind.label;
        let fail = |helper: &mut Self, reason: String, started: bool| helper.fail(&reason, started);
        sending();
        let sent = self.stdin.as_mut().map_or_else(
            || {
                Err(std::io::Error::other(format!(
                    "the {label} helper input is closed"
                )))
            },
            |stdin| {
                stdin
                    .write_all(line)
                    .and_then(|()| stdin.write_all(b"\n"))
                    .and_then(|()| stdin.flush())
            },
        );
        if let Err(error) = sent {
            return Err(fail(
                self,
                format!("could not send the {label} request: {error}"),
                false,
            ));
        }
        let _timer = controlfreak_core::timing::start(kind.wait_stage);
        let started = Instant::now();
        let mut skipped = 0;
        loop {
            if control.is_cancelled() && self.stdin.is_some() {
                // EOF cancels the work inside the helper. Keep draining until it replies,
                // exits, or reaches its deadline; a cancelled helper is never reused.
                self.stdin = None;
            }
            match self.replies.recv_timeout(POLL_INTERVAL) {
                // Only JSON objects are replies. A bounded number of other lines, such as a
                // test harness banner, is skipped rather than parsed as a corrupt reply.
                Ok(ReplyLine::Line(bytes))
                    if bytes.trim_ascii_start().first() != Some(&b'{')
                        && skipped < MAX_SKIPPED_LINES =>
                {
                    skipped += 1;
                }
                Ok(ReplyLine::Line(bytes)) => {
                    let reply = serde_json::from_slice::<R>(&bytes);
                    return match reply {
                        Ok(reply) if reply.id() == id => {
                            if let Err(error) = control.check(kind.operation) {
                                let _ = terminate(&mut self.child);
                                return Err(RequestFailure {
                                    error,
                                    started: true,
                                });
                            }
                            Ok(reply)
                        }
                        Ok(reply) => Err(fail(
                            self,
                            format!(
                                "the isolated {label} process answered request {} instead of {id}",
                                reply.id()
                            ),
                            true,
                        )),
                        Err(error) => Err(fail(
                            self,
                            format!(
                                "the isolated {label} process returned invalid output: {error}"
                            ),
                            true,
                        )),
                    };
                }
                Ok(ReplyLine::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Err(error) = control.check(kind.operation) {
                        let _ = terminate(&mut self.child);
                        return Err(RequestFailure {
                            error,
                            started: true,
                        });
                    }
                    let status = self.wait_for_exit();
                    return Err(fail(self, exited_reason(label, status), false));
                }
                Ok(ReplyLine::Failed(reason)) => {
                    return Err(fail(
                        self,
                        format!("could not read {label} stdout: {reason}"),
                        true,
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() >= timeout => {
                    return Err(fail(
                        self,
                        format!(
                            "the isolated {label} process timed out after {} ms",
                            timeout.as_millis()
                        ),
                        true,
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Ends the helper and describes why the request failed, with its last stderr output.
    fn fail(&mut self, reason: &str, started: bool) -> RequestFailure {
        let cleanup = terminate(&mut self.child);
        let stderr = self.stderr_text();
        RequestFailure {
            error: (self.kind.error)(format!("{reason}{stderr}{cleanup}")),
            started,
        }
    }

    fn wait_for_exit(&mut self) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + HELPER_EXIT_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                _ => return None,
            }
        }
    }

    fn stderr_text(&self) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (front, back) = tail.as_slices();
        let text = String::from_utf8_lossy(&[front, back].concat())
            .trim()
            .to_owned();
        if text.is_empty() {
            String::new()
        } else {
            format!(": {text}")
        }
    }
}

impl Drop for WarmHelper {
    fn drop(&mut self) {
        // EOF asks an idle helper to exit; force it only if it does not within the grace period.
        self.stdin = None;
        if self.wait_for_exit().is_none() {
            let _ = terminate(&mut self.child);
        }
    }
}

fn exited_reason(label: &str, status: Option<std::process::ExitStatus>) -> String {
    let status = status.and_then(|status| status.code()).map_or_else(
        || "without reporting an exit code".to_owned(),
        |code| {
            let unsigned = u32::from_ne_bytes(code.to_ne_bytes());
            format!("with exit code 0x{unsigned:08x}")
        },
    );
    format!("the isolated {label} process exited {status} before replying")
}

fn terminate(child: &mut Child) -> String {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return String::new();
    }
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    match (kill_error, wait_error) {
        (None, None) => String::new(),
        (Some(kill), None) => format!("; cleanup kill reported: {kill}"),
        (None, Some(wait)) => format!("; cleanup wait failed: {wait}"),
        (Some(kill), Some(wait)) => {
            format!("; cleanup kill reported: {kill}; cleanup wait failed: {wait}")
        }
    }
}

/// Reads one `\n`-terminated line without its terminator, refusing lines longer than `limit`.
/// Returns `None` at end of input.
pub(super) fn read_bounded_line(
    input: &mut impl BufRead,
    limit: u64,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let (chunk, found) = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((available.len(), false), |index| (index, true));
        if line.len() as u64 + chunk as u64 > limit {
            return Err(std::io::Error::other(format!(
                "a line exceeded {limit} bytes"
            )));
        }
        line.extend_from_slice(&available[..chunk]);
        input.consume(chunk + usize::from(found));
        if found {
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_bounded_line;

    #[test]
    fn bounded_lines_split_requests_and_refuse_oversized_ones() {
        let mut input = std::io::Cursor::new(b"first\nsecond\nlast".to_vec());
        assert_eq!(read_bounded_line(&mut input, 6).unwrap().unwrap(), b"first");
        assert_eq!(
            read_bounded_line(&mut input, 6).unwrap().unwrap(),
            b"second"
        );
        assert_eq!(read_bounded_line(&mut input, 6).unwrap().unwrap(), b"last");
        assert!(read_bounded_line(&mut input, 6).unwrap().is_none());
        let mut oversized = std::io::Cursor::new(b"seven!!\n".to_vec());
        assert!(read_bounded_line(&mut oversized, 6).is_err());
    }
}
