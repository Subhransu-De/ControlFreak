//! A short-lived process that asks Windows once to bring one window to the foreground.
//!
//! Windows grants `SetForegroundWindow` to a process only while no user input has arrived since
//! that process started (or when it received the last input itself). The server runs for the
//! whole client session and the user always types the request after it starts, so its own calls
//! are refused from then on, however often they are retried. A process started after that input
//! is still allowed, so when the server's request is refused, `focus_window` starts this helper
//! from the server's own executable. It makes the same documented call once, for the window the
//! server already validated, and exits. It sends no input and touches no other window; the
//! server validates the foreground again on every poll afterwards, exactly as without it.

use super::{
    HWND, c_void,
    desktop::{activate_window, window_process_id},
    process::spawn_managed_child,
};
use controlfreak_core::ActivationHelperOutcome;
use std::{
    ffi::OsString,
    io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(crate) const FLAG: &str = "--activation-helper";

/// Exit codes of the helper process.
const ACCEPTED: i32 = 0;
const REFUSED: i32 = 1;
const USAGE: i32 = 2;
const WINDOW_CHANGED: i32 = 3;

/// The helper normally exits within tens of milliseconds. This bound keeps a stuck helper from
/// holding the input lock; on expiry it is terminated with its job.
pub(super) const HELPER_TIMEOUT: Duration = Duration::from_millis(1_500);
const POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    window: usize,
    process_id: u32,
}

impl Request {
    /// `None` when the arguments are not a helper invocation at all.
    pub(crate) fn parse(arguments: &[String]) -> io::Result<Option<Self>> {
        if arguments.first().is_none_or(|argument| argument != FLAG) {
            return Ok(None);
        }
        let invalid = |reason: &str| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{FLAG} requires --window HWND --process PID: {reason}"),
            )
        };
        let [_, window_flag, window, process_flag, process] = arguments else {
            return Err(invalid("wrong number of arguments"));
        };
        if window_flag != "--window" || process_flag != "--process" {
            return Err(invalid("unexpected argument names"));
        }
        let window = window
            .parse::<usize>()
            .map_err(|_| invalid("the window handle is not a number"))?;
        let process_id = process
            .parse::<u32>()
            .map_err(|_| invalid("the process ID is not a number"))?;
        if window == 0 || process_id == 0 {
            return Err(invalid("the window handle and process ID must be non-zero"));
        }
        Ok(Some(Self { window, process_id }))
    }

    fn arguments(self) -> [OsString; 5] {
        [
            FLAG.into(),
            "--window".into(),
            self.window.to_string().into(),
            "--process".into(),
            self.process_id.to_string().into(),
        ]
    }

    fn hwnd(self) -> HWND {
        HWND(std::ptr::with_exposed_provenance_mut::<c_void>(self.window))
    }
}

/// Runs inside the helper process and returns its exit code. The window must still belong to the
/// process the server validated, so a handle reused by another program is never activated.
pub(crate) fn serve(request: Request) -> i32 {
    let hwnd = request.hwnd();
    if window_process_id(hwnd) != Some(request.process_id) {
        return WINDOW_CHANGED;
    }
    if activate_window(hwnd) {
        ACCEPTED
    } else {
        REFUSED
    }
}

/// Exit code for a helper invocation whose arguments did not parse.
pub(crate) const fn usage_exit_code() -> i32 {
    USAGE
}

/// Starts the helper for `hwnd`, owned by `process_id`, and waits a bounded time for its answer.
pub(super) fn request_activation(
    program: &Path,
    hwnd: HWND,
    process_id: u32,
    timeout: Duration,
) -> ActivationHelperOutcome {
    let request = Request {
        window: hwnd.0.expose_provenance(),
        process_id,
    };
    let Ok(child) = spawn_managed_child(program, &request.arguments()) else {
        return ActivationHelperOutcome::Failed;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return outcome(status.code()),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            // Timed out or unobservable: the job closes with `child` and takes the helper with it.
            Ok(None) | Err(_) => {
                let _ = child.terminate();
                return ActivationHelperOutcome::Failed;
            }
        }
    }
}

fn outcome(code: Option<i32>) -> ActivationHelperOutcome {
    match code {
        Some(ACCEPTED) => ActivationHelperOutcome::Accepted,
        Some(REFUSED) => ActivationHelperOutcome::Refused,
        _ => ActivationHelperOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|&argument| argument.to_owned()).collect()
    }

    #[test]
    fn other_invocations_are_not_helper_requests() {
        assert_eq!(Request::parse(&[]).unwrap(), None);
        assert_eq!(Request::parse(&args(&["--ocr-helper"])).unwrap(), None);
    }

    #[test]
    fn a_request_round_trips_through_its_arguments() {
        let request = Request {
            window: 0x0004_0A2C,
            process_id: 4242,
        };
        let arguments: Vec<String> = request
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(Request::parse(&arguments).unwrap(), Some(request));
    }

    #[test]
    fn malformed_requests_are_refused() {
        for bad in [
            &[FLAG][..],
            &[FLAG, "--window", "12", "--process"],
            &[FLAG, "--process", "12", "--window", "34"],
            &[FLAG, "--window", "x", "--process", "34"],
            &[FLAG, "--window", "12", "--process", "-1"],
            &[FLAG, "--window", "0", "--process", "34"],
            &[FLAG, "--window", "12", "--process", "0"],
            &[FLAG, "--window", "12", "--process", "34", "extra"],
        ] {
            assert!(Request::parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        assert_eq!(outcome(Some(ACCEPTED)), ActivationHelperOutcome::Accepted);
        assert_eq!(outcome(Some(REFUSED)), ActivationHelperOutcome::Refused);
        for other in [Some(USAGE), Some(WINDOW_CHANGED), Some(101), None] {
            assert_eq!(outcome(other), ActivationHelperOutcome::Failed);
        }
    }

    #[test]
    fn a_stale_window_is_never_activated() {
        // A handle that names no window: the helper reports a change without calling Windows.
        let request = Request {
            window: 0x7FFF_FFF0,
            process_id: std::process::id(),
        };
        assert_eq!(serve(request), WINDOW_CHANGED);
    }

    #[test]
    fn a_helper_that_cannot_start_fails_without_waiting() {
        let started = Instant::now();
        let outcome = request_activation(
            Path::new(r"C:\definitely\missing\controlfreak.exe"),
            HWND(std::ptr::with_exposed_provenance_mut::<c_void>(0x10)),
            1,
            Duration::from_secs(5),
        );
        assert_eq!(outcome, ActivationHelperOutcome::Failed);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
