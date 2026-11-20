#![deny(unsafe_code)]

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use controlfreak_core::{PlatformBackend, PlatformError, SecurityContext};

#[cfg(not(target_os = "windows"))]
compile_error!("ControlFreak v0.1.0-alpha supports Windows only");

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
#[path = "windows/installer_files.rs"]
mod installer_files;

#[cfg(target_os = "windows")]
pub use installer_files::copy_configuration_permissions;

#[cfg(target_os = "windows")]
#[path = "windows/installer_processes.rs"]
mod installer_processes;

#[cfg(target_os = "windows")]
pub use installer_processes::{InstallerBlocker, installer_blockers};

#[cfg(all(target_os = "windows", feature = "packaging-tests"))]
#[path = "windows/packaging_tests.rs"]
pub mod packaging_tests;

#[cfg(target_os = "windows")]
use windows as implementation;

#[cfg(target_os = "windows")]
pub use implementation::{DesktopArbitrator, ManagedChild, SessionSignals, UserStop};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrHelperCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
}

impl OcrHelperCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    fn program(&self) -> &Path {
        &self.program
    }

    fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

#[cfg(target_os = "windows")]
pub fn backend() -> Box<dyn PlatformBackend> {
    Box::new(implementation::Backend::new())
}

#[cfg(target_os = "windows")]
pub fn backend_with_security(security_context: SecurityContext) -> Box<dyn PlatformBackend> {
    Box::new(implementation::Backend::with_security(security_context))
}

/// The production backend: OCR runs in `ocr_helper`, UI Automation queries run in
/// `server_executable --uia-helper`, and a refused foreground request in `focus_window` is asked
/// once more through `server_executable --activation-helper`.
#[cfg(target_os = "windows")]
pub fn backend_with_helpers_and_security(
    ocr_helper: OcrHelperCommand,
    server_executable: PathBuf,
    security_context: SecurityContext,
) -> Box<dyn PlatformBackend> {
    Box::new(
        implementation::Backend::with_ocr_helper_and_security(ocr_helper, security_context)
            .with_uia_helper(OcrHelperCommand::new(&server_executable).arg("--uia-helper"))
            .with_activation_helper(server_executable),
    )
}

#[cfg(target_os = "windows")]
pub fn server_security_context(
    elevated_operation_allowed: bool,
) -> Result<SecurityContext, PlatformError> {
    implementation::server_security_context(elevated_operation_allowed)
}

#[cfg(target_os = "windows")]
pub fn desktop_arbitrator() -> Result<implementation::DesktopArbitrator, String> {
    implementation::DesktopArbitrator::start()
}

#[cfg(target_os = "windows")]
pub fn spawn_helper_process(
    program: &Path,
    arguments: &[OsString],
) -> std::io::Result<ManagedChild> {
    implementation::spawn_helper_process(program, arguments)
}

#[cfg(target_os = "windows")]
pub fn serve_desktop_glow_helper(arguments: &[String]) -> std::io::Result<Option<()>> {
    implementation::serve_desktop_glow_helper(arguments)
}

/// Runs the one-shot foreground helper when `arguments` ask for it and returns its exit code.
/// It writes nothing to stdout.
#[cfg(target_os = "windows")]
#[must_use]
pub fn serve_activation_helper(arguments: &[String]) -> Option<i32> {
    implementation::serve_activation_helper(arguments)
}

#[cfg(target_os = "windows")]
pub fn serve_ocr_helper(
    input: impl std::io::Read + Send + 'static,
    output: impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    implementation::serve_ocr_helper(input, output)
}

/// Serves `--uia-helper`: read-only UI Automation queries, one JSON line each, until input ends.
#[cfg(target_os = "windows")]
pub fn serve_uia_helper(
    input: impl std::io::Read + Send + 'static,
    output: impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    implementation::serve_uia_helper(input, output)
}
