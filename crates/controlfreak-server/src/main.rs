#![forbid(unsafe_code)]

use std::{
    env,
    error::Error,
    fmt, io,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use controlfreak_core::{PlatformBackend, SecurityContext};
use controlfreak_platform::OcrHelperCommand;

#[cfg(target_os = "windows")]
struct PlatformArbitrator(controlfreak_platform::DesktopArbitrator);

#[cfg(target_os = "windows")]
impl controlfreak_mcp::ActivityArbitrator for PlatformArbitrator {
    fn try_acquire(&self) -> Result<(), controlfreak_mcp::ArbitrationError> {
        match self.0.try_acquire() {
            Ok(true) => Ok(()),
            Ok(false) => Err(controlfreak_mcp::ArbitrationBusy {
                owner_instance_id: None,
                retry_after_ms: 500,
            }
            .into()),
            Err(reason) => Err(controlfreak_mcp::ArbitrationError::Unavailable { reason }),
        }
    }

    fn release(&self) {
        self.0.release();
    }
}

#[cfg(target_os = "windows")]
mod desktop_glow;

const HELP: &str = "ControlFreak native Windows computer-use MCP server

Usage:
  controlfreak                       Run the local STDIO MCP server
  controlfreak --allow-elevated      Explicitly allow an elevated server token
  controlfreak --print-capabilities  Print the platform capability declaration
  controlfreak --print-tools         Print tool descriptions and JSON schemas
  controlfreak --help                Show this help
  controlfreak --version             Show the version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    OcrHelper,
    PrintCapabilities,
    PrintTools,
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Options {
    command: Command,
    allow_elevated: bool,
}

#[derive(Debug)]
struct ElevatedStartupError(SecurityContext);

impl fmt::Display for ElevatedStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ControlFreak inherited an elevated {} token; restart with --allow-elevated only if elevated desktop control is intended",
            self.0.windows_integrity_level
        )
    }
}

impl Error for ElevatedStartupError {}

#[tokio::main]
async fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|panic| {
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "server_panic",
                "process_id": std::process::id(),
                "message": panic.to_string(),
                "location": panic.location().map(std::string::ToString::to_string),
            })
        );
    }));
    match try_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            print_structured_startup_error(error.as_ref());
            ExitCode::FAILURE
        }
    }
}

async fn try_main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    #[cfg(target_os = "windows")]
    if controlfreak_platform::serve_desktop_glow_helper(&arguments)?.is_some() {
        return Ok(());
    }
    let options = parse_options(arguments)?;
    match options.command {
        Command::Serve => run_server(options.allow_elevated).await,
        Command::OcrHelper => run_ocr_helper(),
        Command::PrintCapabilities => print_capabilities(options.allow_elevated),
        Command::PrintTools => {
            println!(
                "{}",
                serde_json::to_string_pretty(&controlfreak_mcp::tool_definitions())?
            );
            Ok(())
        }
        Command::Help => {
            println!("{HELP}");
            Ok(())
        }
        Command::Version => {
            println!("controlfreak {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn parse_options(arguments: impl IntoIterator<Item = String>) -> io::Result<Options> {
    let mut allow_elevated = false;
    let mut command = None;
    for argument in arguments {
        if argument == "--allow-elevated" {
            allow_elevated = true;
            continue;
        }
        let next = match argument.as_str() {
            "--ocr-helper" => Command::OcrHelper,
            "--print-capabilities" => Command::PrintCapabilities,
            "--print-tools" => Command::PrintTools,
            "--help" | "-h" => Command::Help,
            "--version" | "-V" => Command::Version,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument '{argument}'\n\n{HELP}"),
                ));
            }
        };
        if command.replace(next).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("only one command may be specified\n\n{HELP}"),
            ));
        }
    }
    let command = command.unwrap_or(Command::Serve);
    if allow_elevated
        && matches!(
            command,
            Command::OcrHelper | Command::PrintTools | Command::Help | Command::Version
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "--allow-elevated is valid only for the server or --print-capabilities\n\n{HELP}"
            ),
        ));
    }
    Ok(Options {
        command,
        allow_elevated,
    })
}

async fn run_server(allow_elevated: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
    #[cfg(target_os = "windows")]
    {
        let security_context = controlfreak_platform::server_security_context(allow_elevated)?;
        enforce_elevated_startup_policy(security_context)?;
        let elevated = security_context.elevated;
        let safety_indicator = controlfreak_mcp::SafetyIndicator::dormant();
        let arbitrator = Arc::new(PlatformArbitrator(
            controlfreak_platform::desktop_arbitrator()?,
        ));
        let stop = controlfreak_core::StopController::default();
        let signals = Arc::new(controlfreak_platform::SessionSignals::new(elevated));
        // The session tab and its Stop button start with the first screen read or the first
        // session, not with the server; the Esc monitor starts with the first session only. A
        // failure to start is a refused session with a reason, never a server that does not
        // start: read-only tools keep working and ignore a tab that cannot start. The next
        // observation or session retries.
        let session_tab: Arc<Mutex<Option<controlfreak_platform::UserStop>>> =
            Arc::new(Mutex::new(None));
        let tab_stop = stop.clone();
        let tab_signals = Arc::clone(&signals);
        let tab_indicator = safety_indicator.clone();
        let start_tab = {
            let session_tab = Arc::clone(&session_tab);
            let tab_stop = tab_stop.clone();
            move || {
                ensure_started(&session_tab, || {
                    let indicator = tab_indicator.clone();
                    controlfreak_platform::UserStop::start_tab(
                        &tab_stop,
                        Arc::clone(&tab_signals),
                        move |reason| indicator.report_failure(reason),
                    )
                })
            }
        };
        if !desktop_glow::disabled_for_tests() {
            let announce = Arc::clone(&signals);
            let start_tab = start_tab.clone();
            start_tab_on_first_observation(&stop, move || {
                announce.announce_latest_read();
                start_tab()
            });
        }
        let glow_stop = stop.clone();
        controlfreak_mcp::serve_stdio_with_indicator_and_arbitrator(
            platform_backend(security_context)?,
            safety_indicator,
            move || {
                // Debug builds under test run without any desktop indicator, so the test suite
                // never creates windows or a keyboard hook unless it asks for them.
                if desktop_glow::disabled_for_tests() {
                    return desktop_glow::DesktopGlow::start(
                        elevated,
                        glow_stop.clone(),
                        Arc::clone(&signals),
                    );
                }
                start_tab()
                    .and_then(|()| {
                        // Only a session installs the Esc keyboard hook.
                        session_tab
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_mut()
                            .map_or(Ok(()), |tab| tab.attach_session_monitor(&tab_stop))
                    })
                    .map_err(|reason| {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "event": "session_tab_unavailable",
                                "reason": reason,
                            })
                        );
                        format!("the session tab and Stop button could not start: {reason}")
                    })?;
                desktop_glow::DesktopGlow::start(elevated, glow_stop.clone(), Arc::clone(&signals))
            },
            arbitrator,
            stop,
        )
        .await
    }

    #[cfg(not(target_os = "windows"))]
    controlfreak_mcp::serve_stdio(controlfreak_platform::backend()).await
}

/// Starts the tab, off the caller's thread, the first time an observation tool reads the screen
/// before any session has started it, so a client that only captures and reads text still sees
/// the "reading the screen" state. It claims no session and installs no keyboard hook, and a
/// failure is logged and tolerated: read-only tools never depend on the indicator. At most one
/// start runs at a time, and a failed start is retried by the next observation only.
fn start_tab_on_first_observation(
    stop: &controlfreak_core::StopController,
    start: impl Fn() -> Result<(), String> + Send + Sync + 'static,
) {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let start = Arc::new(start);
    let started = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicBool::new(false));
    let attempted = Arc::new(AtomicU64::new(0));
    let counter = stop.clone();
    stop.subscribe(move || {
        let count = counter.observation_count();
        if count > attempted.fetch_max(count, Ordering::AcqRel)
            && !started.load(Ordering::Acquire)
            && !in_flight.swap(true, Ordering::AcqRel)
        {
            let (start, started, in_flight) =
                (start.clone(), Arc::clone(&started), Arc::clone(&in_flight));
            let spawned = std::thread::Builder::new()
                .name("controlfreak-reading-tab".to_owned())
                .spawn({
                    let in_flight = Arc::clone(&in_flight);
                    move || {
                        match start() {
                            Ok(()) => started.store(true, Ordering::Release),
                            Err(reason) => eprintln!(
                                "{}",
                                serde_json::json!({
                                    "event": "reading_tab_unavailable",
                                    "reason": reason,
                                })
                            ),
                        }
                        in_flight.store(false, Ordering::Release);
                    }
                });
            if spawned.is_err() {
                in_flight.store(false, Ordering::Release);
            }
        }
        true
    });
}

/// Starts `value` into `slot` if it is not there yet. A failure leaves the slot empty, so the
/// next call tries again.
#[cfg(target_os = "windows")]
fn ensure_started<T, E>(
    slot: &Mutex<Option<T>>,
    start: impl FnOnce() -> Result<T, E>,
) -> Result<(), E> {
    let mut slot = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_none() {
        *slot = Some(start()?);
    }
    Ok(())
}

fn run_ocr_helper() -> Result<(), Box<dyn Error + Send + Sync>> {
    controlfreak_platform::serve_ocr_helper(io::stdin(), io::stdout().lock())
}

fn platform_backend(
    security_context: SecurityContext,
) -> Result<Box<dyn PlatformBackend>, Box<dyn Error + Send + Sync>> {
    let helper = OcrHelperCommand::new(env::current_exe()?).arg("--ocr-helper");
    Ok(controlfreak_platform::backend_with_ocr_helper_and_security(
        helper,
        security_context,
    ))
}

fn print_capabilities(allow_elevated: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
    let security_context = controlfreak_platform::server_security_context(allow_elevated)?;
    let backend: Box<dyn PlatformBackend> =
        controlfreak_platform::backend_with_security(security_context);
    println!("{}", serde_json::to_string_pretty(&backend.report())?);
    Ok(())
}

fn enforce_elevated_startup_policy(
    security_context: SecurityContext,
) -> Result<(), ElevatedStartupError> {
    if security_context.elevated && !security_context.elevated_operation_allowed {
        Err(ElevatedStartupError(security_context))
    } else {
        Ok(())
    }
}

fn print_structured_startup_error(error: &(dyn Error + 'static)) {
    eprintln!("{}", structured_startup_error(error));
}

fn structured_startup_error(error: &(dyn Error + 'static)) -> serde_json::Value {
    error.downcast_ref::<ElevatedStartupError>().map_or_else(
        || {
            serde_json::json!({
                "event": "startup_failed",
                "error": {
                    "code": "server_startup_failed",
                    "message": error.to_string(),
                }
            })
        },
        |policy| {
            serde_json::json!({
                "event": "startup_refused",
                "error": {
                    "code": "elevated_operation_requires_opt_in",
                    "message": policy.to_string(),
                    "details": {
                        "server_elevated": true,
                        "windows_integrity_level": policy.0.windows_integrity_level,
                        "required_option": "--allow-elevated",
                    }
                }
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use controlfreak_core::IntegrityLevel;

    fn context(elevated: bool, allowed: bool) -> SecurityContext {
        SecurityContext {
            elevated,
            windows_integrity_level: if elevated {
                IntegrityLevel::High
            } else {
                IntegrityLevel::Medium
            },
            elevated_operation_allowed: allowed,
        }
    }

    /// The first screen read starts the tab on its own thread and claims nothing; a failed start
    /// is tolerated and retried only by a later observation, never by session changes.
    #[test]
    fn the_first_observation_starts_the_tab_without_a_session_and_tolerates_failure() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let stop = controlfreak_core::StopController::default();
        let attempts = Arc::new(AtomicU32::new(0));
        let (sender, receiver) = std::sync::mpsc::channel();
        let sender = Mutex::new(sender);
        let counted = Arc::clone(&attempts);
        start_tab_on_first_observation(&stop, move || {
            let attempt = counted.fetch_add(1, Ordering::SeqCst) + 1;
            sender.lock().unwrap().send(attempt).unwrap();
            if attempt == 1 {
                Err("Direct2D is unavailable".to_owned())
            } else {
                Ok(())
            }
        });
        let wait = || receiver.recv_timeout(std::time::Duration::from_secs(10));

        // Nothing starts for stop-controller changes that are not screen reads.
        stop.set_session_active(true);
        stop.set_session_active(false);
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 0);

        // The first read asks for the tab. It claims no session; the failure breaks nothing.
        stop.note_observation();
        assert_eq!(wait().unwrap(), 1);
        assert!(
            !stop.session_active(),
            "an observation never claims a session"
        );
        assert!(!stop.is_stopped());

        // Session changes do not retry a failed start; the next read does, and then it is done.
        stop.set_session_active(true);
        stop.set_session_active(false);
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        stop.note_observation();
        assert_eq!(wait().unwrap(), 2);
        std::thread::sleep(std::time::Duration::from_millis(100));
        stop.note_observation();
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err()
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_session_tab_starts_once_and_a_failed_start_is_retried_not_fatal() {
        let slot: Mutex<Option<u32>> = Mutex::new(None);
        let mut starts = 0;
        let failed: Result<(), String> = ensure_started(&slot, || {
            starts += 1;
            Err("Direct2D is unavailable".to_owned())
        });
        assert_eq!(failed.unwrap_err(), "Direct2D is unavailable");
        assert!(
            slot.lock().unwrap().is_none(),
            "nothing is kept after a failure"
        );
        assert_eq!(starts, 1);

        ensure_started::<_, String>(&slot, || {
            starts += 1;
            Ok(7)
        })
        .unwrap();
        assert_eq!(*slot.lock().unwrap(), Some(7), "the retry succeeds");
        ensure_started::<_, String>(&slot, || {
            starts += 1;
            Ok(8)
        })
        .unwrap();
        assert_eq!(starts, 2, "a running tab is not started again");
        assert_eq!(*slot.lock().unwrap(), Some(7));
    }

    #[test]
    fn standard_token_startup_is_allowed() {
        assert!(enforce_elevated_startup_policy(context(false, false)).is_ok());
    }

    #[test]
    fn elevated_startup_requires_explicit_opt_in() {
        let error = enforce_elevated_startup_policy(context(true, false))
            .expect_err("elevated startup must be refused by default");
        assert!(error.to_string().contains("--allow-elevated"));
        let structured = structured_startup_error(&error);
        assert_eq!(
            structured["error"]["code"],
            "elevated_operation_requires_opt_in"
        );
        assert_eq!(
            structured["error"]["details"]["required_option"],
            "--allow-elevated"
        );
    }

    #[test]
    fn elevated_startup_with_opt_in_is_allowed() {
        assert!(enforce_elevated_startup_policy(context(true, true)).is_ok());
    }

    #[test]
    fn command_line_opt_in_is_explicit() {
        let options = parse_options(["--allow-elevated".to_owned()]).expect("valid options");
        assert_eq!(options.command, Command::Serve);
        assert!(options.allow_elevated);
    }

    #[test]
    fn tool_export_is_a_standalone_command() {
        let options = parse_options(["--print-tools".to_owned()]).unwrap();
        assert_eq!(options.command, Command::PrintTools);
        assert!(!options.allow_elevated);
        for other in ["--print-capabilities", "--help", "--allow-elevated"] {
            assert!(parse_options(["--print-tools".to_owned(), other.to_owned()]).is_err());
        }
    }
}
