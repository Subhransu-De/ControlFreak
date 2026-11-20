//! Developer-only packaging commands. Never shipped in the installer.
mod e2e;
mod lifecycle;
mod package;
mod process;
mod release;
mod verify;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(about = "Build, verify and test ControlFreak Windows distribution artifacts")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Print the workspace version, optionally validating a release tag.
    Version {
        #[arg(long)]
        tag: Option<String>,
    },
    /// Prepare a release version and move Unreleased changes into a dated section.
    PrepareRelease {
        /// The version, with or without a leading v. Empty or omitted picks the next patch
        /// version, or the release of a prerelease.
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        date: String,
    },
    /// Validate the versioned changelog and write release notes.
    ReleaseNotes {
        #[arg(long)]
        output: PathBuf,
    },
    /// Fetch and verify the pinned portable Inno Setup compiler.
    Compiler {
        #[arg(long)]
        output: PathBuf,
    },
    /// Package existing release binaries as an EXE and ZIP.
    Package {
        #[arg(long)]
        compiler: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        require_sbom: bool,
        #[arg(long)]
        test_setup: bool,
    },
    /// Validate checksums, archive contents and packaged MCP startup.
    Verify {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// The commit the package must report, with a clean tree. Release builds pass the tag's
        /// commit.
        #[arg(long)]
        expect_revision: Option<String>,
    },
    /// Exercise installation in synthetic profiles using a separate test identity.
    TestInstaller {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        production_installer: bool,
    },
    /// Publish verified artifacts from the GitHub Actions release workflow.
    Publish {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        tag: String,
    },
    /// Build the e2e binaries and run the live suites on a disposable desktop.
    ///
    /// Arguments go to `controlfreak-e2e run`, for example `--strict`.
    E2e {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let metadata = package::Metadata::load()?;
    match cli.command {
        Task::Version { tag } => {
            metadata.validate_tag(tag.as_deref())?;
            println!("{}", metadata.version()?);
        }
        Task::PrepareRelease { version, date } => {
            release::prepare(&metadata, version.as_deref(), &date)?;
        }
        Task::ReleaseNotes { output } => {
            let changelog = std::fs::read_to_string(metadata.workspace_root.join("CHANGELOG.md"))?;
            std::fs::write(output, release::notes(&changelog, metadata.version()?)?)?;
        }
        Task::Compiler { output } => println!("{}", package::compiler(&output)?.display()),
        Task::Package {
            compiler,
            output,
            tag,
            require_sbom,
            test_setup,
        } => {
            metadata.validate_tag(tag.as_deref())?;
            package::build(&metadata, &compiler, &output, require_sbom, test_setup)?;
        }
        Task::Verify {
            package,
            output,
            expect_revision,
        } => {
            verify::package(
                &package,
                &output,
                metadata.version()?,
                expect_revision.as_deref(),
            )?;
        }
        Task::TestInstaller {
            package,
            output,
            production_installer,
        } => lifecycle::run(&package, &output, metadata.version()?, production_installer)?,
        Task::Publish { package, tag } => {
            metadata.validate_tag(Some(&tag))?;
            package::publish(&package, &tag, &metadata)?;
        }
        Task::E2e { args } => e2e::run(&metadata.workspace_root, &args)?,
    }
    Ok(())
}
