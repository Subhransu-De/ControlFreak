use crate::{Result, process};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fmt::Write,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use zip::{ZipWriter, write::SimpleFileOptions};

pub const BINARIES: [&str; 2] = ["controlfreak.exe", "controlfreak-installer.exe"];
pub const DOCUMENTS: [&str; 5] = [
    "README.md",
    "CHANGELOG.md",
    "SECURITY.md",
    "LICENSE",
    "Cargo.lock",
];
const COMPILER_HASH: &str = "4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0";

#[derive(Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub target_directory: PathBuf,
    pub workspace_root: PathBuf,
}
#[derive(Deserialize)]
pub struct Package {
    pub name: String,
    version: String,
    pub manifest_path: PathBuf,
}

impl Metadata {
    pub fn load() -> Result<Self> {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = process::checked(
            Command::new(cargo)
                .current_dir(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .ok_or("Missing workspace root")?,
                )
                .args(["metadata", "--no-deps", "--locked", "--format-version", "1"]),
            Duration::from_mins(1),
        )?;
        Ok(serde_json::from_str(&output.stdout)?)
    }
    pub fn version(&self) -> Result<&str> {
        let version = &self
            .packages
            .first()
            .ok_or("Workspace has no packages")?
            .version;
        if self.packages.iter().any(|p| &p.version != version) {
            return Err("Workspace versions differ".into());
        }
        Ok(version)
    }
    pub fn validate_tag(&self, tag: Option<&str>) -> Result<()> {
        if let Some(tag) = tag
            && tag != format!("v{}", self.version()?)
        {
            return Err("Release tag does not match workspace version".into());
        }
        Ok(())
    }
}

pub fn fresh(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    if path.exists() {
        return Err("Use a new output directory to exclude stale artifacts".into());
    }
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}

pub fn compiler(output: &Path) -> Result<PathBuf> {
    let output = fresh(output)?;
    let installer = output.join("innosetup-6.7.1.exe");
    // curl.exe is supplied by supported Windows versions; invoke it directly.
    process::checked(Command::new("curl.exe").args([
        "--fail", "--location", "--silent", "--show-error", "--proto", "=https",
        "--proto-redir", "=https", "--max-time", "120", "--output",
    ]).arg(&installer).arg("https://github.com/jrsoftware/issrc/releases/download/is-6_7_1/innosetup-6.7.1.exe"),
        Duration::from_secs(130))?;
    if hash(&installer)? != COMPILER_HASH {
        return Err("Compiler checksum mismatch".into());
    }
    let compiler = output.join("compiler");
    process::checked(
        Command::new(&installer)
            .args([
                "/PORTABLE=1",
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/CURRENTUSER",
            ])
            .arg(format!("/DIR={}", compiler.display())),
        Duration::from_mins(2),
    )?;
    let executable = compiler.join("ISCC.exe");
    if !executable.is_file() {
        return Err("Portable compiler was not extracted".into());
    }
    Ok(executable)
}

pub fn archive_name(version: &str) -> String {
    format!("controlfreak-v{version}-x86_64-pc-windows-msvc.zip")
}
pub fn installer_name(version: &str) -> String {
    format!("ControlFreak-{version}.exe")
}

pub fn build(
    metadata: &Metadata,
    compiler: &Path,
    output: &Path,
    require_sbom: bool,
    test_setup: bool,
) -> Result<()> {
    let version = metadata.version()?;
    let compiler = std::path::absolute(compiler)?;
    if !compiler.is_file() {
        return Err("Inno Setup compiler not found".into());
    }
    let output = fresh(output)?;
    let payload = output.join("payload");
    fs::create_dir_all(payload.join("sbom"))?;
    for name in BINARIES {
        fs::copy(
            metadata.target_directory.join("release").join(name),
            payload.join(name),
        )?;
    }
    for name in DOCUMENTS {
        fs::copy(metadata.workspace_root.join(name), payload.join(name))?;
    }
    fs::write(payload.join("version.txt"), format!("{version}\n"))?;
    let mut count = 0;
    // xtask is developer tooling, never part of the installed payload or its SBOM set.
    for package in metadata.packages.iter().filter(|p| p.name != "xtask") {
        let sbom = package
            .manifest_path
            .parent()
            .ok_or("Invalid manifest path")?
            .join("controlfreak.cdx.json");
        if sbom.is_file() {
            fs::copy(
                sbom,
                payload
                    .join("sbom")
                    .join(format!("{}.cdx.json", package.name)),
            )?;
            count += 1;
        } else if require_sbom {
            return Err(format!("Missing crate SBOM: {}", package.name).into());
        }
    }
    if count == 0 {
        fs::write(
            payload.join("sbom/NOT-GENERATED.txt"),
            "Development package: SBOM generation was not requested.\n",
        )?;
    }
    let numeric = format!(
        "{}.0",
        version.split(['-', '+']).next().ok_or("Invalid version")?
    );
    let mut command = Command::new(compiler);
    command
        .arg("/Qp")
        .arg(format!("/DVersion={version}"))
        .arg(format!("/DNumericVersion={numeric}"))
        .arg(format!("/DPayloadDir={}", payload.display()))
        .arg(format!("/DOutputPath={}", output.display()));
    if test_setup {
        command.arg("/DTestSetup=1");
    }
    command.arg(
        metadata
            .workspace_root
            .join("packaging/windows/controlfreak.iss"),
    );
    process::checked(&mut command, Duration::from_mins(3))?;
    let archive = output.join(archive_name(version));
    let mut zip = ZipWriter::new(File::create(&archive)?);
    zip_directory(&mut zip, &payload, &payload)?;
    zip.finish()?;
    for artifact in [archive, output.join(installer_name(version))] {
        let name = artifact
            .file_name()
            .ok_or("Missing artifact name")?
            .to_string_lossy();
        fs::write(
            output.join(format!("{name}.sha256")),
            format!("{}  {name}\n", hash(&artifact)?),
        )?;
    }
    println!("Windows installer, portable ZIP and checksums built.");
    Ok(())
}

pub fn publish(directory: &Path, tag: &str, version: &str) -> Result<()> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || std::env::var("GITHUB_REF").as_deref() != Ok(format!("refs/tags/{tag}").as_str())
    {
        return Err("Publication is restricted to the tag release workflow".into());
    }
    let mut command = Command::new("gh");
    command.args([
        "release",
        "create",
        tag,
        "--verify-tag",
        "--generate-notes",
        "--title",
        tag,
    ]);
    if version.contains('-') {
        command.arg("--prerelease");
    }
    for name in [archive_name(version), installer_name(version)] {
        let artifact = directory.join(&name);
        let checksum = directory.join(format!("{name}.sha256"));
        if fs::read_to_string(&checksum)?.trim() != format!("{}  {name}", hash(&artifact)?) {
            return Err("Publication checksum mismatch".into());
        }
        command.arg(artifact).arg(checksum);
    }
    command.args(["--notes", "Windows x64: download the EXE installer for installation and optional MCP client configuration, or the ZIP for portable use. These binaries are unsigned. SHA-256 checksums are included; each package contains license information and SBOMs."]);
    process::checked(&mut command, Duration::from_mins(3))?;
    println!("GitHub release published.");
    Ok(())
}

fn zip_directory(zip: &mut ZipWriter<File>, root: &Path, directory: &Path) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            zip_directory(zip, root, &path)?;
        } else {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("Non-UTF-8 payload filename")?
                .replace('\\', "/");
            zip.start_file(
                relative,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )?;
            std::io::copy(&mut File::open(path)?, zip)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tag_must_exactly_match_workspace() {
        let metadata = Metadata {
            packages: vec![Package {
                name: "fixture".into(),
                version: "1.2.3-alpha".into(),
                manifest_path: PathBuf::new(),
            }],
            target_directory: PathBuf::new(),
            workspace_root: PathBuf::new(),
        };
        assert!(metadata.validate_tag(Some("v1.2.3-alpha")).is_ok());
        assert!(metadata.validate_tag(Some("v1.2.3")).is_err());
    }
    #[test]
    fn archive_roundtrip_preserves_unicode_paths_and_bytes() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        fs::create_dir_all(payload.join("日本語")).unwrap();
        fs::write(payload.join("日本語/fixture.txt"), b"synthetic").unwrap();
        let archive = root.path().join("fixture.zip");
        let mut writer = ZipWriter::new(File::create(&archive).unwrap());
        zip_directory(&mut writer, &payload, &payload).unwrap();
        writer.finish().unwrap();
        let mut reader = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        reader.extract(root.path().join("extracted")).unwrap();
        assert_eq!(
            fs::read(root.path().join("extracted/日本語/fixture.txt")).unwrap(),
            b"synthetic"
        );
        assert!(fresh(&payload).is_err());
    }
}
