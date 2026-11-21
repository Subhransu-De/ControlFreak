#![forbid(unsafe_code)]

use std::{
    env,
    error::Error,
    fmt, io,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use controlfreak_core::{BuildInfo, PlatformBackend, SecurityContext};
use controlfreak_mcp::Profile;
use controlfreak_platform::OcrHelperCommand;

#[cfg(target_os = "windows")]
struct PlatformArbitrator(controlfreak_platform::DesktopArbitrator);

#[cfg(target_os = "windows")]
impl controlfreak_mcp::ActivityArbitrator for PlatformArbitrator {
    fn try_acquire(&self) -> Result<(), controlfreak_mcp::ArbitrationError> {
        match self.0.try_acquire() {
            Ok(true) => Ok(()),
            Ok(false) => Err(controlfreak_mcp::ArbitrationBusy {
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
  controlfreak --profile observe     Serve observation tools only: no session, focus or input
  controlfreak --tools a,b,c         Serve only these tools (names from --print-tools)
  controlfreak --print-capabilities  Print the platform capability declaration
  controlfreak --print-tools         Print tool descriptions and the full JSON schemas
  controlfreak --help                Show this help
  controlfreak --version             Show the version

--profile and --tools may be combined; every --tools name must then be a tool that
--profile observe serves. get_server_status, end_control_session and stop_desktop_work
are served in every profile. An invalid value stops the server at startup.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    OcrHelper,
    UiaHelper,
    PrintCapabilities,
    PrintTools,
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    command: Command,
    allow_elevated: bool,
    /// The tools the server serves: every tool unless `--profile` or `--tools` narrows them.
    profile: Profile,
}

/// Longest part of a rejected argument that a startup error repeats.
const MAX_ECHOED_ARGUMENT_CHARS: usize = 64;

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
    #[cfg(target_os = "windows")]
    if let Some(code) = controlfreak_platform::serve_activation_helper(&arguments) {
        std::process::exit(code);
    }
    let options = parse_options(arguments)?;
    match options.command {
        Command::Serve => run_server(options.allow_elevated, options.profile).await,
        Command::OcrHelper => run_ocr_helper(),
        Command::UiaHelper => run_uia_helper(),
        Command::PrintCapabilities => print_capabilities(options.allow_elevated),
        Command::PrintTools => {
            println!(
                "{}",
                serde_json::to_string_pretty(&controlfreak_mcp::full_tool_definitions())?
            );
            Ok(())
        }
        Command::Help => {
            println!("{HELP}");
            Ok(())
        }
        Command::Version => {
            println!("{}", version_text(&build_info()));
            Ok(())
        }
    }
}

fn invalid_input(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// At most [`MAX_ECHOED_ARGUMENT_CHARS`] characters of `value`, so an error stays bounded.
fn shortened(value: &str) -> String {
    value.chars().take(MAX_ECHOED_ARGUMENT_CHARS).collect()
}

/// The value of option `name`, given as `name=value` (`inline`) or as the next argument. A next
/// argument that starts with `--` is another option, so the value is missing.
fn option_value(
    name: &str,
    inline: Option<&str>,
    arguments: &mut impl Iterator<Item = String>,
) -> io::Result<String> {
    match inline.map(str::to_owned).or_else(|| arguments.next()) {
        Some(value) if !value.starts_with("--") => Ok(value),
        _ => Err(invalid_input(format!("{name} needs a value\n\n{HELP}"))),
    }
}

fn parse_options(arguments: impl IntoIterator<Item = String>) -> io::Result<Options> {
    let mut allow_elevated = false;
    let mut command = None;
    let mut profile_name: Option<String> = None;
    let mut tool_list: Option<String> = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--allow-elevated" {
            allow_elevated = true;
            continue;
        }
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        if name == "--profile" || name == "--tools" {
            let value = option_value(name, inline, &mut arguments)?;
            let slot = if name == "--profile" {
                &mut profile_name
            } else {
                &mut tool_list
            };
            if slot.replace(value).is_some() {
                return Err(invalid_input(format!(
                    "{name} may be given only once\n\n{HELP}"
                )));
            }
            continue;
        }
        let next = match argument.as_str() {
            "--ocr-helper" => Command::OcrHelper,
            "--uia-helper" => Command::UiaHelper,
            "--print-capabilities" => Command::PrintCapabilities,
            "--print-tools" => Command::PrintTools,
            "--help" | "-h" => Command::Help,
            "--version" | "-V" => Command::Version,
            _ => {
                return Err(invalid_input(format!(
                    "unknown argument '{}'\n\n{HELP}",
                    shortened(&argument)
                )));
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
            Command::OcrHelper
                | Command::UiaHelper
                | Command::PrintTools
                | Command::Help
                | Command::Version
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "--allow-elevated is valid only for the server or --print-capabilities\n\n{HELP}"
            ),
        ));
    }
    if (profile_name.is_some() || tool_list.is_some()) && command != Command::Serve {
        return Err(invalid_input(format!(
            "--profile and --tools are valid only for the server\n\n{HELP}"
        )));
    }
    let profile = match profile_name.as_deref() {
        None => Profile::full(),
        Some("observe") => Profile::observe(),
        Some(other) => {
            return Err(invalid_input(format!(
                "unknown --profile '{}'; the only profile is observe",
                shortened(other)
            )));
        }
    };
    // A malformed list stops startup: the server never falls back to serving every tool.
    let profile = match tool_list {
        Some(list) => profile
            .restrict_to(&list)
            .map_err(|error| invalid_input(error.to_string()))?,
        None => profile,
    };
    Ok(Options {
        command,
        allow_elevated,
        profile,
    })
}

async fn run_server(
    allow_elevated: bool,
    profile: Profile,
) -> Result<(), Box<dyn Error + Send + Sync>> {
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
        let backend = platform_backend(security_context)?;
        // Sets awareness now rather than at the first tool call. A refusal does not stop startup:
        // captures and input report it, and get_server_status shows it as degraded.
        if let Some(status) = backend.dpi_awareness()
            && let Some(reason) = status.reason
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "dpi_awareness_unsupported",
                    "awareness": status.awareness,
                    "reason": reason,
                })
            );
        }
        controlfreak_mcp::serve_stdio_with_indicator_and_arbitrator(
            backend,
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
            build_info(),
            profile,
        )
        .await
    }

    #[cfg(not(target_os = "windows"))]
    controlfreak_mcp::serve_stdio(controlfreak_platform::backend(), build_info(), profile).await
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
        // Once the tab is up this listener has nothing left to do, so it removes itself.
        !started.load(Ordering::Acquire)
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

fn run_uia_helper() -> Result<(), Box<dyn Error + Send + Sync>> {
    controlfreak_platform::serve_uia_helper(io::stdin(), io::stdout().lock())
}

fn platform_backend(
    security_context: SecurityContext,
) -> Result<Box<dyn PlatformBackend>, Box<dyn Error + Send + Sync>> {
    let executable = env::current_exe()?;
    let helper = OcrHelperCommand::new(&executable).arg("--ocr-helper");
    Ok(controlfreak_platform::backend_with_helpers_and_security(
        helper,
        executable,
        security_context,
    ))
}

fn print_capabilities(allow_elevated: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
    let security_context = controlfreak_platform::server_security_context(allow_elevated)?;
    // The same helpers as the server, so the report probes OCR the way the server runs it.
    let backend = platform_backend(security_context)?;
    // The server starts the session lamp with every session; this process starts none.
    let report = backend
        .report()
        .with_build(build_info())
        .with_lamp(controlfreak_core::Readiness::Ready);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// The build identity that build.rs embedded.
fn build_info() -> BuildInfo {
    let dirty = match env!("CONTROLFREAK_DIRTY") {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    BuildInfo::new(
        env!("CARGO_PKG_VERSION"),
        env!("CONTROLFREAK_REVISION"),
        dirty,
        env!("CONTROLFREAK_TARGET"),
    )
}

/// The `--version` text. The first line stays `controlfreak <version>` for scripts that read it.
fn version_text(build: &BuildInfo) -> String {
    let dirty = build
        .dirty
        .map_or("unknown", |dirty| if dirty { "true" } else { "false" });
    format!(
        "controlfreak {}\nrevision: {}\ndirty: {dirty}\ntarget: {}",
        build.version, build.revision, build.target
    )
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

    /// #8: the embedded identity has its documented format and names no path, user or host.
    #[test]
    fn the_build_identity_carries_no_path_user_or_host_name() {
        let build = build_info();
        assert!(build.is_well_formed(), "{build:?}");
        assert_eq!(build.version, env!("CARGO_PKG_VERSION"));
        for field in [&build.version, &build.revision, &build.target] {
            assert!(
                !field.contains(['\\', '/', ':', '@', ' ']),
                "{field} looks like a path, an address or a timestamp"
            );
        }
        let lower = version_text(&build).to_ascii_lowercase();
        assert!(!lower.contains(&env!("CARGO_MANIFEST_DIR").to_ascii_lowercase()));
        // The real account and host names can collide with the triple (a user named `windows`),
        // so the format check is tested with synthetic ones in every field instead.
        let target = build.target.as_str();
        for (field, value) in [
            ("revision", r"C:\Users\cf-test-user\src"),
            ("revision", "cf-test-user@cf-test-host"),
            ("revision", "cf-test-host"),
            ("version", "0.2.3+cf-test-user@cf-test-host"),
            ("version", "0.2.3 cf-test-user"),
            ("target", "/home/cf-test-user/target"),
            ("target", "cf-test-host:x86_64"),
        ] {
            let mut leaked = BuildInfo::new(&build.version, &build.revision, None, target);
            match field {
                "revision" => leaked.revision = value.to_owned(),
                "version" => leaked.version = value.to_owned(),
                _ => leaked.target = value.to_owned(),
            }
            assert!(!leaked.is_well_formed(), "{field} = {value} passed");
        }
    }

    /// The revision is the commit the test binary was built from, so build.rs reruns when the
    /// commit changes. Skipped in a source archive or without Git.
    #[test]
    fn the_embedded_revision_is_the_checked_out_commit() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !workspace.join(".git").exists() {
            return;
        }
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["rev-parse", "HEAD"])
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let head = String::from_utf8(output.stdout).unwrap();
        let build = build_info();
        assert_eq!(build.revision, head.trim());
        assert!(build.dirty.is_some());
    }

    #[test]
    fn the_version_text_keeps_its_first_line() {
        let build = BuildInfo::new("1.2.3", "unknown", None, "x86_64-pc-windows-msvc");
        assert_eq!(
            version_text(&build),
            "controlfreak 1.2.3\nrevision: unknown\ndirty: unknown\ntarget: x86_64-pc-windows-msvc"
        );
        let build = BuildInfo::new(
            "1.2.3",
            "a".repeat(40),
            Some(false),
            "x86_64-pc-windows-msvc",
        );
        assert_eq!(version_text(&build).lines().nth(2), Some("dirty: false"));
    }

    fn parse(arguments: &[&str]) -> io::Result<Options> {
        parse_options(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn no_profile_flag_serves_every_tool() {
        assert_eq!(parse(&[]).unwrap().profile, Profile::full());
    }

    #[test]
    fn profile_and_tool_flags_take_both_forms_and_combine() {
        let observe = parse(&["--profile", "observe"]).unwrap();
        assert_eq!(observe.command, Command::Serve);
        assert_eq!(observe.profile, Profile::observe());
        assert_eq!(
            parse(&["--profile=observe", "--allow-elevated"])
                .unwrap()
                .profile,
            Profile::observe()
        );
        let listed = Profile::full()
            .restrict_to("type_text,list_windows")
            .unwrap();
        assert_eq!(
            parse(&["--tools", "type_text,list_windows"])
                .unwrap()
                .profile,
            listed
        );
        assert_eq!(
            parse(&["--tools=type_text,list_windows"]).unwrap().profile,
            listed
        );
        // Combined, in either order: the list narrows the observe profile.
        let both = Profile::observe().restrict_to("list_windows").unwrap();
        assert_eq!(
            parse(&["--tools", "list_windows", "--profile", "observe"])
                .unwrap()
                .profile,
            both
        );
        assert_eq!(
            parse(&["--profile", "observe", "--tools", "list_windows"])
                .unwrap()
                .profile,
            both
        );
    }

    #[test]
    fn malformed_profile_flags_are_refused_with_a_bounded_message() {
        let long = "x".repeat(10_000);
        for arguments in [
            vec!["--profile"],
            vec!["--profile", "--tools", "list_windows"],
            vec!["--profile", "full"],
            vec!["--profile", "Observe"],
            vec!["--profile="],
            vec!["--profile", "observe", "--profile", "observe"],
            vec!["--tools"],
            vec!["--tools="],
            vec!["--tools", ""],
            vec!["--tools", "list_windows,"],
            vec!["--tools", "list_windows,,type_text"],
            vec!["--tools", "typo_text"],
            vec!["--tools", "type_text,type_text"],
            vec!["--tools", "list_windows", "--tools", "type_text"],
            vec!["--tools", long.as_str()],
            vec!["--profile", long.as_str()],
            vec![long.as_str()],
            // Contradictory: observe never serves input.
            vec!["--profile", "observe", "--tools", "type_text"],
            vec!["--tools", "focus_window", "--profile", "observe"],
            // Only the server takes a profile.
            vec!["--print-tools", "--profile", "observe"],
            vec!["--print-capabilities", "--tools", "list_windows"],
            vec!["--version", "--tools", "list_windows"],
        ] {
            let error = parse(&arguments).expect_err(&format!("{arguments:?}"));
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            let message = error.to_string();
            assert!(
                message.len() < 2048,
                "{arguments:?}: {} bytes",
                message.len()
            );
            assert!(!message.contains(&"x".repeat(65)), "{arguments:?}");
            let structured = structured_startup_error(&error);
            assert_eq!(structured["error"]["code"], "server_startup_failed");
        }
    }
}
