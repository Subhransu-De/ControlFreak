//! Embeds the application manifest that makes `controlfreak.exe` per-monitor v2 DPI aware before
//! any of its code runs. The server also sets awareness at run time and refuses work when Windows
//! keeps it in another mode, so a build without the manifest is still safe, only less robust.

use std::{env, path::Path};

fn main() {
    let manifest = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"))
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
