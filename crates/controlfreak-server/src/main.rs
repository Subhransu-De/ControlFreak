#![forbid(unsafe_code)]

use std::{env, error::Error, fmt, io, process::ExitCode, sync::Arc};

use controlfreak_core::{PlatformBackend, SecurityContext};
use controlfreak_platform::OcrHelperCommand;

#[cfg(target_os = "windows")]
struct PlatformArbitrator(controlfreak_platform::DesktopArbitrator);

#[cfg(target_os = "windows")]
impl controlfreak_mcp::ActivityArbitrator for PlatformArbitrator {
    fn try_acquire(&self) -> Result<(), controlfreak_mcp::ArbitrationBusy> {
        match self.0.try_acquire() {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(controlfreak_mcp::ArbitrationBusy {
                owner_instance_id: None,
                retry_after_ms: 500,
            }),
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
  controlfreak --help                Show this help
  controlfreak --version             Show the version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    OcrHelper,
    PrintCapabilities,
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
            Command::OcrHelper | Command::Help | Command::Version
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
        controlfreak_mcp::serve_stdio_with_indicator_and_arbitrator(
            platform_backend(security_context)?,
            safety_indicator,
            move || desktop_glow::DesktopGlow::start(elevated),
            arbitrator,
        )
        .await
    }

    #[cfg(not(target_os = "windows"))]
    controlfreak_mcp::serve_stdio(controlfreak_platform::backend()).await
}

fn run_ocr_helper() -> Result<(), Box<dyn Error + Send + Sync>> {
    controlfreak_platform::serve_ocr_helper(io::stdin().lock(), io::stdout().lock())
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
}
