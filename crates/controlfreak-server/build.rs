//! Embeds the application manifest that makes `controlfreak.exe` per-monitor v2 DPI aware before
//! any of its code runs. The server also sets awareness at run time and refuses work when Windows
//! keeps it in another mode, so a build without the manifest is still safe, only less robust.
//!
//! Also embeds the build identity: the Git revision, whether tracked files differed from it, and
//! the target triple. A checkout without `.git`, such as a source archive, takes the revision from
//! `CONTROLFREAK_BUILD_REVISION` or reports `unknown`, with an unknown dirty marker. Nothing here
//! reads the clock, and no path, user name or host name reaches the executable.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const REVISION_VARIABLE: &str = "CONTROLFREAK_BUILD_REVISION";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    embed_manifest(&manifest_dir);
    embed_build_identity(&manifest_dir);
}

fn embed_manifest(manifest_dir: &Path) {
    let manifest = manifest_dir
        .join("windows")
        .join("controlfreak.exe.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let target_msvc = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    // A Linux host cross-checking the Windows target links with lld-link, which needs mt.exe or
    // libxml2 to merge manifests. Release builds and tests run on Windows with link.exe.
    let windows_host = env::var("HOST").is_ok_and(|host| host.contains("windows"));
    if target_msvc && windows_host {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
        // The manifest declares the asInvoker level itself, so the linker adds no second one.
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
    }
}

fn embed_build_identity(manifest_dir: &Path) {
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("the server crate is two levels below the workspace root");
    println!("cargo:rerun-if-env-changed={REVISION_VARIABLE}");
    // Only a repository rooted at this workspace counts, never one that happens to contain an
    // extracted source archive.
    let (revision, dirty) = if workspace.join(".git").exists()
        && let Some((revision, dirty)) = git_identity(workspace)
    {
        (revision, if dirty { "true" } else { "false" })
    } else {
        (fallback_revision(), "unknown")
    };
    println!("cargo:rustc-env=CONTROLFREAK_REVISION={revision}");
    println!("cargo:rustc-env=CONTROLFREAK_DIRTY={dirty}");
    let target = env::var("TARGET").expect("set by Cargo");
    assert!(
        target.bytes().all(|byte| byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'_' | b'-')),
        "unexpected target triple"
    );
    println!("cargo:rustc-env=CONTROLFREAK_TARGET={target}");
}

/// The commit and whether tracked files differ from it, or `None` when Git is missing or refuses
/// the repository. Untracked files do not count, as with `git describe --dirty`: Cargo compiles
/// only files that a tracked file names.
fn git_identity(workspace: &Path) -> Option<(String, bool)> {
    let revision = git(workspace, &["rev-parse", "--verify", "HEAD"])?;
    if !is_commit_hash(&revision) {
        return None;
    }
    // Without optional locks, status does not refresh the index, which would rerun this script.
    let status = git(
        workspace,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )?;
    watch_git_state(workspace);
    Some((revision, !status.is_empty()))
}

/// Reruns this script when the commit, the branch, the index or a compiled source changes, so an
/// incremental build reports the state it was built from. A change to a tracked file that no crate
/// compiles, such as documentation, shows only at the next build that reruns this script.
fn watch_git_state(workspace: &Path) {
    let git_dir = git(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    );
    let common_dir = git(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let mut watched = vec![
        workspace.join("crates"),
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
    ];
    if let Some(git_dir) = git_dir.map(PathBuf::from) {
        watched.extend(["HEAD", "index", "logs/HEAD"].map(|name| git_dir.join(name)));
    }
    if let Some(common_dir) = common_dir.map(PathBuf::from) {
        watched.push(common_dir.join("packed-refs"));
        if let Some(branch) = git(workspace, &["symbolic-ref", "-q", "HEAD"]) {
            watched.push(common_dir.join(branch));
        }
    }
    // Cargo reruns on every build for a watched path that does not exist.
    for path in watched.iter().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn git(workspace: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_owned())
}

fn fallback_revision() -> String {
    match env::var(REVISION_VARIABLE) {
        Ok(value) if !value.trim().is_empty() => {
            let value = value.trim().to_ascii_lowercase();
            assert!(
                is_commit_hash(&value),
                "{REVISION_VARIABLE} must be a full 40 or 64 digit hex commit hash"
            );
            value
        }
        _ => "unknown".to_owned(),
    }
}

/// A full SHA-1 or SHA-256 Git object name in lowercase hex, as `controlfreak_core` checks it.
fn is_commit_hash(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
