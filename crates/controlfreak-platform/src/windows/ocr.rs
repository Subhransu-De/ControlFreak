use super::recognize_text_in_process;
use crate::OcrHelperCommand;
use controlfreak_core::{MutationControl, OcrRegionRequest, OcrResult, PlatformError};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    thread,
    time::{Duration, Instant},
};

const OCR_HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const OCR_HELPER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OcrHelperResponse {
    Success { result: OcrResult },
    Error { error: PlatformError },
}

pub(super) fn recognize_text_with_helper(
    command: &OcrHelperCommand,
    request: &OcrRegionRequest,
    control: &MutationControl,
) -> Result<OcrResult, PlatformError> {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    control.check("windows_ocr_helper")?;
    let request_json =
        serde_json::to_vec(request).map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("could not encode the OCR request: {error}"),
        })?;
    let mut child = Command::new(command.program())
        .args(command.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("could not start the isolated OCR process: {error}"),
        })?;

    let send_error = match child.stdin.as_mut() {
        Some(stdin) => stdin
            .write_all(&request_json)
            .and_then(|()| stdin.write_all(b"\n"))
            .err()
            .map(|error| format!("could not send the OCR request: {error}")),
        None => Some("the isolated OCR process did not expose stdin".to_owned()),
    };
    if let Some(reason) = send_error {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("{reason}{cleanup}"),
        });
    }

    let output = wait_for_ocr_helper_controlled(child, OCR_HELPER_TIMEOUT, control)?;
    if !output.status.success() {
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: ocr_helper_failure(output.status, &output.stderr),
        });
    }

    let response: OcrHelperResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process returned invalid output: {error}"),
        })?;
    match response {
        OcrHelperResponse::Success { result } => Ok(result),
        OcrHelperResponse::Error { error } => Err(error),
    }
}

#[cfg(test)]
pub(super) fn wait_for_ocr_helper(
    child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, PlatformError> {
    wait_for_ocr_helper_controlled(child, timeout, &MutationControl::default())
}

pub(super) fn wait_for_ocr_helper_controlled(
    mut child: std::process::Child,
    timeout: Duration,
    control: &MutationControl,
) -> Result<std::process::Output, PlatformError> {
    let Some(stdout) = child.stdout.take() else {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process did not expose stdout{cleanup}"),
        });
    };
    let Some(stderr) = child.stderr.take() else {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process did not expose stderr{cleanup}"),
        });
    };
    let stdout_reader = match thread::Builder::new()
        .name("controlfreak-ocr-stdout".to_owned())
        .spawn(move || read_ocr_pipe(stdout))
    {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = terminate_ocr_helper(&mut child);
            return Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason: format!("could not start the OCR stdout reader: {error}{cleanup}"),
            });
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("controlfreak-ocr-stderr".to_owned())
        .spawn(move || read_ocr_pipe(stderr))
    {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = terminate_ocr_helper(&mut child);
            let _ = stdout_reader.join();
            return Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason: format!("could not start the OCR stderr reader: {error}{cleanup}"),
            });
        }
    };
    let started = Instant::now();
    let process_result = loop {
        if control.is_cancelled() {
            // EOF requests cooperative cancellation inside the owned helper. Keep
            // draining it until native recognition returns or its existing deadline expires.
            drop(child.stdin.take());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(
                    OCR_HELPER_POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())),
                );
            }
            Ok(None) => {
                let cleanup = terminate_ocr_helper(&mut child);
                break Err(format!(
                    "the isolated OCR process timed out after {} seconds{cleanup}",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let cleanup = terminate_ocr_helper(&mut child);
                break Err(format!(
                    "could not poll the isolated OCR process: {error}{cleanup}"
                ));
            }
        }
    };

    let stdout = join_ocr_pipe(stdout_reader, "stdout");
    let stderr = join_ocr_pipe(stderr_reader, "stderr");
    control.check("windows_ocr_helper")?;
    match (process_result, stdout, stderr) {
        (Ok(status), Ok(stdout), Ok(stderr)) => Ok(std::process::Output {
            status,
            stdout,
            stderr,
        }),
        (Err(reason), _, _) | (_, Err(reason), _) | (_, _, Err(reason)) => {
            Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason,
            })
        }
    }
}

fn read_ocr_pipe(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_ocr_pipe(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("the OCR {stream} reader panicked"))?
        .map_err(|error| format!("could not read OCR {stream}: {error}"))
}

fn terminate_ocr_helper(child: &mut std::process::Child) -> String {
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

pub(crate) fn serve_ocr_helper(
    input: impl Read + Send + 'static,
    output: impl Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_ocr_helper_with(input, output, recognize_text_in_process)
}

fn serve_ocr_helper_with(
    input: impl Read + Send + 'static,
    output: impl Write,
    recognize: impl FnOnce(&OcrRegionRequest, &MutationControl) -> Result<OcrResult, PlatformError>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut input = BufReader::new(input);
    let mut request_json = String::new();
    input.read_line(&mut request_json)?;
    let request: OcrRegionRequest = serde_json::from_str(&request_json)?;
    let control = MutationControl::default();
    let cancellation = control.clone();
    // The helper exits after its one response. This reader owns no desktop data;
    // closing stdin requests cancellation even while WinRT recognition is active.
    thread::Builder::new()
        .name("controlfreak-ocr-cancel".to_owned())
        .spawn(move || {
            let _ = input.read(&mut [0_u8; 1]);
            cancellation.cancel();
        })?;
    let response = match recognize(&request, &control) {
        Ok(result) => OcrHelperResponse::Success { result },
        Err(error) => OcrHelperResponse::Error { error },
    };
    serde_json::to_writer(output, &response)?;
    Ok(())
}

fn ocr_helper_failure(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let status = status.code().map_or_else(
        || "without an exit code".to_owned(),
        |code| {
            let unsigned = u32::from_ne_bytes(code.to_ne_bytes());
            format!("with exit code 0x{unsigned:08x}")
        },
    );
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("the isolated OCR process exited {status}")
    } else {
        let tail_start = stderr
            .char_indices()
            .rev()
            .nth(1_999)
            .map_or(0, |(index, _)| index);
        format!(
            "the isolated OCR process exited {status}: {}",
            &stderr[tail_start..]
        )
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn stdin_closure_cancels_recognition_without_killing_the_helper() {
        let input = std::io::Cursor::new(b"{\"display_id\":\"fixture\",\"x\":0,\"y\":0,\"width\":1,\"height\":1,\"language\":null}\n".to_vec());
        let mut output = Vec::new();
        let started = Instant::now();
        serve_ocr_helper_with(input, &mut output, |_, control| {
            control.wait("synthetic_ocr", Duration::from_secs(30))?;
            panic!("helper ignored stdin cancellation");
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            serde_json::from_slice::<OcrHelperResponse>(&output).unwrap(),
            OcrHelperResponse::Error { .. }
        ));
    }
}
