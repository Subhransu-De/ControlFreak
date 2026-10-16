//! Isolated OCR helper processes.
//!
//! Windows OCR runs in a child process so a native fault cannot take down the server. Starting a
//! process and loading the `WinRT` OCR runtime costs tens of milliseconds, so one helper is kept
//! warm between requests. A helper serves one request at a time; closing its stdin cancels the
//! request in progress and ends it. A helper is retired after an error, timeout, cancellation, or
//! `MAX_REQUESTS_PER_HELPER` requests, and exits by itself when idle for `HELPER_IDLE_EXIT`.

use super::recognize_text_in_process;
use crate::OcrHelperCommand;
use controlfreak_core::{MutationControl, OcrRegionRequest, OcrResult, PlatformError};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const OCR_HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const OCR_HELPER_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// An idle helper exits by itself, so a quiet server does not keep the OCR runtime loaded.
const HELPER_IDLE_EXIT: Duration = Duration::from_mins(1);
/// Bounds how long one helper process, and any state it accumulates, is reused.
const MAX_REQUESTS_PER_HELPER: u32 = 100;
// Recognition of a full 8K display stays far below this; a larger reply means a faulty helper.
const MAX_OCR_REPLY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OCR_REQUEST_BYTES: u64 = 64 * 1024;
const STDERR_TAIL_BYTES: usize = 4 * 1024;
const HELPER_EXIT_GRACE: Duration = Duration::from_secs(1);
const MAX_SKIPPED_LINES: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OcrHelperResponse {
    Success { result: OcrResult },
    Error { error: PlatformError },
}

/// One request line sent to a helper.
#[derive(Debug, Serialize, Deserialize)]
struct OcrHelperRequest {
    id: u64,
    #[serde(flatten)]
    request: OcrRegionRequest,
}

/// One helper reply line: the response plus content-free stage timings measured in the helper.
#[derive(Debug, Serialize, Deserialize)]
struct OcrHelperReply {
    id: u64,
    #[serde(flatten)]
    response: OcrHelperResponse,
    #[serde(default)]
    stages_micros: Vec<(String, u64)>,
}

/// Re-records helper stages under server-side names so they are not confused with in-process work.
fn record_helper_stages(stages: &[(String, u64)]) {
    for (stage, micros) in stages {
        let name = match stage.as_str() {
            "ocr_recognize" => "ocr_helper_recognize",
            "ocr_engine_create" => "ocr_helper_engine_create",
            "capture_gdi" => "ocr_helper_capture",
            _ => continue,
        };
        controlfreak_core::timing::record(name, Duration::from_micros(*micros));
    }
}

fn helper_error(reason: impl Into<String>) -> PlatformError {
    PlatformError::OperationFailed {
        operation: "windows_ocr_helper".to_owned(),
        reason: reason.into(),
    }
}

/// The backend's warm helper and request numbering.
#[derive(Default)]
pub(super) struct OcrHelperPool {
    idle: Mutex<Option<WarmOcrHelper>>,
    next_id: AtomicU64,
}

pub(super) fn recognize_text_with_helper(
    command: &OcrHelperCommand,
    pool: &OcrHelperPool,
    request: &OcrRegionRequest,
    control: &MutationControl,
) -> Result<OcrResult, PlatformError> {
    control.check("windows_ocr_helper")?;
    let id = pool.next_id.fetch_add(1, Ordering::Relaxed);
    let line = serde_json::to_vec(&OcrHelperRequest {
        id,
        request: request.clone(),
    })
    .map_err(|error| helper_error(format!("could not encode the OCR request: {error}")))?;

    // A concurrent request finds the slot empty and starts its own helper.
    let idle = pool
        .idle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let (mut helper, reused) = match idle.and_then(WarmOcrHelper::if_running) {
        Some(helper) => (helper, true),
        None => (WarmOcrHelper::spawn(command, MAX_OCR_REPLY_BYTES)?, false),
    };
    let reply = match helper.request(id, &line, control, OCR_HELPER_TIMEOUT) {
        Ok(reply) => reply,
        // A reused helper can exit while idle just as a request arrives. OCR only reads the
        // screen, so one retry on a fresh helper cannot repeat a desktop effect.
        Err(failure) if reused && !failure.started && !control.is_cancelled() => {
            drop(helper);
            helper = WarmOcrHelper::spawn(command, MAX_OCR_REPLY_BYTES)?;
            helper
                .request(id, &line, control, OCR_HELPER_TIMEOUT)
                .map_err(|failure| failure.error)?
        }
        Err(failure) => return Err(failure.error),
    };
    helper.served += 1;
    if helper.served < MAX_REQUESTS_PER_HELPER {
        let mut slot = pool
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(helper);
        }
    }
    record_helper_stages(&reply.stages_micros);
    match reply.response {
        OcrHelperResponse::Success { result } => Ok(result),
        OcrHelperResponse::Error { error } => Err(error),
    }
}

/// A failed request. `started` is false only when the helper produced nothing at all for it, so
/// it is unknown whether recognition began; a retry is then safe for a read-only request.
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
struct WarmOcrHelper {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: mpsc::Receiver<ReplyLine>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    served: u32,
}

impl WarmOcrHelper {
    fn spawn(command: &OcrHelperCommand, reply_limit: u64) -> Result<Self, PlatformError> {
        use std::{os::windows::process::CommandExt, process::Stdio};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _timer = controlfreak_core::timing::start("ocr_helper_spawn");
        let mut child = std::process::Command::new(command.program())
            .args(command.arguments())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| {
                helper_error(format!("could not start the isolated OCR process: {error}"))
            })?;
        match Self::attach(&mut child, reply_limit) {
            Ok((stdin, replies, stderr_tail)) => Ok(Self {
                child,
                stdin: Some(stdin),
                replies,
                stderr_tail,
                served: 0,
            }),
            Err(reason) => {
                let cleanup = terminate(&mut child);
                Err(helper_error(format!("{reason}{cleanup}")))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn attach(
        child: &mut Child,
        reply_limit: u64,
    ) -> Result<
        (
            ChildStdin,
            mpsc::Receiver<ReplyLine>,
            Arc<Mutex<VecDeque<u8>>>,
        ),
        String,
    > {
        let stdin = child
            .stdin
            .take()
            .ok_or("the isolated OCR process did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("the isolated OCR process did not expose stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("the isolated OCR process did not expose stderr")?;
        let (sender, replies) = mpsc::channel();
        thread::Builder::new()
            .name("controlfreak-ocr-stdout".to_owned())
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
            .map_err(|error| format!("could not start the OCR stdout reader: {error}"))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        let tail = Arc::clone(&stderr_tail);
        thread::Builder::new()
            .name("controlfreak-ocr-stderr".to_owned())
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
            .map_err(|error| format!("could not start the OCR stderr reader: {error}"))?;
        Ok((stdin, replies, stderr_tail))
    }

    fn is_running(&mut self) -> bool {
        self.stdin.is_some() && matches!(self.child.try_wait(), Ok(None))
    }

    fn if_running(mut self) -> Option<Self> {
        self.is_running().then_some(self)
    }

    fn request(
        &mut self,
        id: u64,
        line: &[u8],
        control: &MutationControl,
        timeout: Duration,
    ) -> Result<OcrHelperReply, RequestFailure> {
        let fail = |helper: &mut Self, reason: String, started: bool| {
            let cleanup = terminate(&mut helper.child);
            let stderr = helper.stderr_text();
            RequestFailure {
                error: helper_error(format!("{reason}{stderr}{cleanup}")),
                started,
            }
        };
        let sent = self.stdin.as_mut().map_or_else(
            || Err(std::io::Error::other("the OCR helper input is closed")),
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
                format!("could not send the OCR request: {error}"),
                false,
            ));
        }
        let _timer = controlfreak_core::timing::start("ocr_helper_wait");
        let started = Instant::now();
        let mut skipped = 0;
        loop {
            if control.is_cancelled() && self.stdin.is_some() {
                // EOF cancels recognition inside the helper. Keep draining until it replies,
                // exits, or reaches its deadline; a cancelled helper is never reused.
                self.stdin = None;
            }
            match self.replies.recv_timeout(OCR_HELPER_POLL_INTERVAL) {
                // Only JSON objects are replies. A bounded number of other lines, such as a
                // test harness banner, is skipped rather than parsed as a corrupt reply.
                Ok(ReplyLine::Line(bytes))
                    if bytes.trim_ascii_start().first() != Some(&b'{')
                        && skipped < MAX_SKIPPED_LINES =>
                {
                    skipped += 1;
                }
                Ok(ReplyLine::Line(bytes)) => {
                    let reply = serde_json::from_slice::<OcrHelperReply>(&bytes);
                    return match reply {
                        Ok(reply) if reply.id == id => {
                            if let Err(error) = control.check("windows_ocr_helper") {
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
                                "the isolated OCR process answered request {} instead of {id}",
                                reply.id
                            ),
                            true,
                        )),
                        Err(error) => Err(fail(
                            self,
                            format!("the isolated OCR process returned invalid output: {error}"),
                            true,
                        )),
                    };
                }
                Ok(ReplyLine::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Err(error) = control.check("windows_ocr_helper") {
                        let _ = terminate(&mut self.child);
                        return Err(RequestFailure {
                            error,
                            started: true,
                        });
                    }
                    let status = self.wait_for_exit();
                    return Err(fail(self, exited_reason(status), false));
                }
                Ok(ReplyLine::Failed(reason)) => {
                    return Err(fail(
                        self,
                        format!("could not read OCR stdout: {reason}"),
                        true,
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() >= timeout => {
                    return Err(fail(
                        self,
                        format!(
                            "the isolated OCR process timed out after {} seconds",
                            timeout.as_secs()
                        ),
                        true,
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn wait_for_exit(&mut self) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + HELPER_EXIT_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(OCR_HELPER_POLL_INTERVAL),
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

impl Drop for WarmOcrHelper {
    fn drop(&mut self) {
        // EOF asks an idle helper to exit; force it only if it does not within the grace period.
        self.stdin = None;
        if self.wait_for_exit().is_none() {
            let _ = terminate(&mut self.child);
        }
    }
}

fn exited_reason(status: Option<std::process::ExitStatus>) -> String {
    let status = status.and_then(|status| status.code()).map_or_else(
        || "without reporting an exit code".to_owned(),
        |code| {
            let unsigned = u32::from_ne_bytes(code.to_ne_bytes());
            format!("with exit code 0x{unsigned:08x}")
        },
    );
    format!("the isolated OCR process exited {status} before replying")
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
fn read_bounded_line(input: &mut impl BufRead, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
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

pub(crate) fn serve_ocr_helper(
    input: impl Read + Send + 'static,
    output: impl Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_ocr_helper_with(input, output, recognize_text_in_process, HELPER_IDLE_EXIT)
}

/// Serves request lines until input ends or stays idle for `idle_exit`. Closing input also
/// cancels the request in progress.
fn serve_ocr_helper_with(
    input: impl Read + Send + 'static,
    mut output: impl Write,
    recognize: impl Fn(&OcrRegionRequest, &MutationControl) -> Result<OcrResult, PlatformError>,
    idle_exit: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let current = Arc::new(Mutex::new(None::<MutationControl>));
    let closed = Arc::new(AtomicBool::new(false));
    let (sender, requests) = mpsc::channel();
    {
        let current = Arc::clone(&current);
        let closed = Arc::clone(&closed);
        // This reader owns no desktop data. End of input cancels recognition in progress.
        thread::Builder::new()
            .name("controlfreak-ocr-input".to_owned())
            .spawn(move || {
                let mut input = BufReader::new(input);
                while let Ok(Some(line)) = read_bounded_line(&mut input, MAX_OCR_REQUEST_BYTES) {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
                closed.store(true, Ordering::Release);
                if let Some(control) = current
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                {
                    control.cancel();
                }
            })?;
    }
    while let Ok(line) = requests.recv_timeout(idle_exit) {
        let request: OcrHelperRequest = serde_json::from_slice(&line)?;
        let control = MutationControl::default();
        *current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(control.clone());
        if closed.load(Ordering::Acquire) {
            control.cancel();
        }
        controlfreak_core::timing::reset();
        let response = match recognize(&request.request, &control) {
            Ok(result) => OcrHelperResponse::Success { result },
            Err(error) => OcrHelperResponse::Error { error },
        };
        *current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let stages_micros = controlfreak_core::timing::take()
            .into_iter()
            .map(|stage| (stage.stage.to_owned(), stage.micros))
            .collect();
        serde_json::to_writer(
            &mut output,
            &OcrHelperReply {
                id: request.id,
                response,
                stages_micros,
            },
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
        if closed.load(Ordering::Acquire) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_line(id: u64) -> Vec<u8> {
        let mut line = serde_json::to_vec(&OcrHelperRequest {
            id,
            request: OcrRegionRequest {
                display_id: "fixture".into(),
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                language: None,
            },
        })
        .unwrap();
        line.push(b'\n');
        line
    }

    fn synthetic_result(language: &str) -> OcrResult {
        serde_json::from_value(serde_json::json!({
            "target_ref": null,
            "display": {"id": "fixture", "name": "fixture",
                        "bounds": {"left": 0, "top": 0, "width": 1, "height": 1},
                        "is_primary": true},
            "source_bounds": {"left": 0, "top": 0, "width": 1, "height": 1},
            "language": language,
            "text": "",
            "lines": [],
        }))
        .unwrap()
    }

    fn replies(output: &[u8]) -> Vec<OcrHelperReply> {
        output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

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

    #[test]
    fn one_helper_serves_sequential_requests_until_input_ends() {
        let (input, mut requests) = std::io::pipe().unwrap();
        let (replies, output) = std::io::pipe().unwrap();
        let helper = thread::spawn(move || {
            serve_ocr_helper_with(
                input,
                output,
                |_, _| Ok(synthetic_result("en-US")),
                Duration::from_secs(5),
            )
            .unwrap();
        });
        let mut replies = BufReader::new(replies);
        for id in [7, 8] {
            requests.write_all(&request_line(id)).unwrap();
            let line = read_bounded_line(&mut replies, 1 << 20).unwrap().unwrap();
            assert_eq!(
                serde_json::from_slice::<OcrHelperReply>(&line).unwrap().id,
                id
            );
        }
        drop(requests);
        helper.join().unwrap();
        assert!(read_bounded_line(&mut replies, 1 << 20).unwrap().is_none());
    }

    #[test]
    fn stdin_closure_cancels_recognition_without_killing_the_helper() {
        let input = std::io::Cursor::new(request_line(1));
        let mut output = Vec::new();
        let started = Instant::now();
        serve_ocr_helper_with(
            input,
            &mut output,
            |_, control| {
                control.wait("synthetic_ocr", Duration::from_secs(30))?;
                panic!("helper ignored stdin cancellation");
            },
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            replies(&output).remove(0).response,
            OcrHelperResponse::Error { .. }
        ));
    }

    #[test]
    fn idle_helpers_exit_by_themselves() {
        let (reader, _keep_open) = std::io::pipe().unwrap();
        let started = Instant::now();
        serve_ocr_helper_with(
            reader,
            Vec::new(),
            |_, _| Ok(synthetic_result("en-US")),
            Duration::from_millis(50),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn test_helper(name: &str) -> OcrHelperCommand {
        OcrHelperCommand::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("--exact")
            .arg(format!("windows::ocr::tests::{name}"))
            .arg("--nocapture")
            .arg("--quiet")
    }

    /// Serves synthetic results naming its process, so callers can see helper reuse.
    #[test]
    #[ignore = "spawned only by warm helper tests"]
    fn warm_echo_test_helper() {
        serve_ocr_helper_with(
            std::io::stdin(),
            std::io::stdout().lock(),
            |_, _| Ok(synthetic_result(&format!("pid-{}", std::process::id()))),
            Duration::from_secs(20),
        )
        .unwrap();
    }

    /// Never finishes a request unless cancelled.
    #[test]
    #[ignore = "spawned only by warm helper tests"]
    fn warm_slow_test_helper() {
        serve_ocr_helper_with(
            std::io::stdin(),
            std::io::stdout().lock(),
            |_, control| {
                control.wait("synthetic_ocr", Duration::from_mins(1))?;
                Ok(synthetic_result("late"))
            },
            Duration::from_mins(1),
        )
        .unwrap();
    }

    /// Replies with a line larger than the caller's limit.
    #[test]
    #[ignore = "spawned only by warm helper tests"]
    fn warm_oversized_test_helper() {
        serve_ocr_helper_with(
            std::io::stdin(),
            std::io::stdout().lock(),
            |_, _| Ok(synthetic_result(&"x".repeat(256 * 1024))),
            Duration::from_secs(20),
        )
        .unwrap();
    }

    fn language(result: &OcrResult) -> String {
        serde_json::to_value(result).unwrap()["language"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn the_pool_reuses_one_helper_process_across_requests() {
        let pool = OcrHelperPool::default();
        let command = test_helper("warm_echo_test_helper");
        let control = MutationControl::default();
        let request = serde_json::from_slice::<OcrHelperRequest>(&request_line(0))
            .unwrap()
            .request;
        let first = recognize_text_with_helper(&command, &pool, &request, &control).unwrap();
        let second = recognize_text_with_helper(&command, &pool, &request, &control).unwrap();
        assert!(language(&first).starts_with("pid-"));
        assert_eq!(language(&first), language(&second));
        assert!(pool.idle.lock().unwrap().is_some());
    }

    #[test]
    fn a_dead_idle_helper_is_replaced_transparently() {
        let pool = OcrHelperPool::default();
        let command = test_helper("warm_echo_test_helper");
        let control = MutationControl::default();
        let request = serde_json::from_slice::<OcrHelperRequest>(&request_line(0))
            .unwrap()
            .request;
        let first = recognize_text_with_helper(&command, &pool, &request, &control).unwrap();
        {
            let mut slot = pool.idle.lock().unwrap();
            let helper = slot.as_mut().unwrap();
            helper.child.kill().unwrap();
            helper.child.wait().unwrap();
        }
        let second = recognize_text_with_helper(&command, &pool, &request, &control).unwrap();
        assert_ne!(language(&first), language(&second));
    }

    #[test]
    fn helpers_are_killed_at_their_deadline_and_not_reused() {
        let command = test_helper("warm_slow_test_helper");
        let mut helper = WarmOcrHelper::spawn(&command, MAX_OCR_REPLY_BYTES).unwrap();
        let started = Instant::now();
        let failure = helper
            .request(
                1,
                &request_line(1)[..request_line(1).len() - 1],
                &MutationControl::default(),
                Duration::from_millis(300),
            )
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(failure.error.to_string().contains("timed out"));
        assert!(!helper.is_running());
    }

    #[test]
    fn cancelled_requests_drain_without_waiting_for_the_deadline() {
        let command = test_helper("warm_slow_test_helper");
        let mut helper = WarmOcrHelper::spawn(&command, MAX_OCR_REPLY_BYTES).unwrap();
        let control = MutationControl::default();
        control.cancel();
        let started = Instant::now();
        let line = request_line(1);
        let failure = helper
            .request(
                1,
                &line[..line.len() - 1],
                &control,
                Duration::from_secs(30),
            )
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(failure.error.to_string().contains("cancelled"));
        assert!(!helper.is_running());
    }

    #[test]
    fn oversized_replies_fail_the_request_and_retire_the_helper() {
        let command = test_helper("warm_oversized_test_helper");
        let mut helper = WarmOcrHelper::spawn(&command, 64 * 1024).unwrap();
        let line = request_line(1);
        let failure = helper
            .request(
                1,
                &line[..line.len() - 1],
                &MutationControl::default(),
                Duration::from_secs(10),
            )
            .err()
            .unwrap();
        assert!(failure.error.to_string().contains("exceeded"));
        assert!(!helper.is_running());
    }
}
