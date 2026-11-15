use crate::Result;
use std::{path::Path, process::Command};

const PACKAGES: [&str; 3] = [
    "controlfreak-server",
    "controlfreak-live-tests",
    "controlfreak-e2e",
];

pub fn run(workspace_root: &Path, args: &[String]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .current_dir(workspace_root)
        .args(["build", "--locked", "--bins"]);
    for package in PACKAGES {
        build.args(["-p", package]);
    }
    if !build.status()?.success() {
        return Err("building the e2e binaries failed".into());
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace_root.join("target"), Into::into);
    let bin_dir = target.join("debug");
    let harness = bin_dir.join(format!("controlfreak-e2e{}", std::env::consts::EXE_SUFFIX));
    let status = Command::new(harness)
        .current_dir(workspace_root)
        .arg("run")
        .arg("--bin-dir")
        .arg(&bin_dir)
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the e2e suites failed ({status})").into())
    }
}
