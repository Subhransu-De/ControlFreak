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
use windows as implementation;

#[cfg(target_os = "windows")]
pub use implementation::{DesktopArbitrator, ManagedChild};

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

#[cfg(target_os = "windows")]
pub fn backend_with_ocr_helper_and_security(
    command: OcrHelperCommand,
    security_context: SecurityContext,
) -> Box<dyn PlatformBackend> {
    Box::new(implementation::Backend::with_ocr_helper_and_security(
        command,
        security_context,
    ))
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

#[cfg(target_os = "windows")]
pub fn serve_ocr_helper(
    input: impl std::io::Read,
    output: impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    implementation::serve_ocr_helper(input, output)
}
