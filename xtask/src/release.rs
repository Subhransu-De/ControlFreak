//! Release changelog validation shared by local preparation and publication.
use crate::{Result, package::Metadata, process};
use std::{fs, process::Command, time::Duration};
use toml_edit::{DocumentMut, value};

pub fn prepare(metadata: &Metadata, version: &str, date: &str) -> Result<()> {
    let manifest_path = metadata.workspace_root.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let changelog_path = metadata.workspace_root.join("CHANGELOG.md");
    let changelog = fs::read_to_string(&changelog_path)?;
    let (manifest, changelog) = prepare_text(&manifest, &changelog, version, date)?;
    // xtask's build need not fetch packages used only by other workspace crates.
    // Populate the complete locked graph before an offline workspace resolution.
    process::checked(
        Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(&metadata.workspace_root)
            .args(["fetch", "--locked"]),
        Duration::from_mins(5),
    )?;
    fs::write(manifest_path, manifest)?;
    fs::write(changelog_path, changelog)?;
    // Cargo updates workspace package versions while keeping locked external
    // dependencies. This command deliberately runs without --locked.
    process::checked(
        Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(&metadata.workspace_root)
            .args(["update", "--workspace", "--offline"]),
        Duration::from_mins(1),
    )?;
    Ok(())
}

fn prepare_text(
    manifest: &str,
    changelog: &str,
    version: &str,
    date: &str,
) -> Result<(String, String)> {
    let next = semver::Version::parse(version)?;
    if next.to_string() != version || !next.build.is_empty() {
        return Err("Use a canonical version without v or build metadata".into());
    }
    let mut manifest: DocumentMut = manifest.parse()?;
    let current = manifest["workspace"]["package"]["version"]
        .as_str()
        .ok_or("Workspace version is missing")?;
    if next < semver::Version::parse(current)? {
        return Err("Release version cannot go backwards".into());
    }
    if changelog
        .lines()
        .any(|line| line.starts_with(&format!("## [{version}]")))
    {
        return Err("This version already has a changelog section; choose a new version".into());
    }
    if changelog
        .lines()
        .filter(|line| *line == "## Unreleased")
        .count()
        != 1
    {
        return Err("Changelog must contain exactly one Unreleased section".into());
    }
    let (preamble, remaining) = changelog
        .split_once("## Unreleased")
        .ok_or("Missing Unreleased section")?;
    let remaining = remaining.trim_start();
    let boundary = if remaining.starts_with("## ") {
        0
    } else {
        remaining
            .find("\n## ")
            .map_or(remaining.len(), |index| index + 1)
    };
    let (changes, history) = remaining.split_at(boundary);
    let changelog = format!(
        "{}## Unreleased\n\n## [{version}] - {date}\n\n{}\n\n{}",
        preamble,
        changes.trim(),
        history
    );
    let changelog = format!("{}\n", changelog.trim_end());
    notes(&changelog, version)?;
    manifest["workspace"]["package"]["version"] = value(version);
    let dependencies = manifest["workspace"]["dependencies"]
        .as_table_like_mut()
        .ok_or("Workspace dependencies are missing")?;
    for (_, dependency) in dependencies.iter_mut() {
        if dependency.get("path").is_some() && dependency.get("version").is_some() {
            dependency["version"] = value(format!("={version}"));
        }
    }
    Ok((manifest.to_string(), changelog))
}

pub fn notes(changelog: &str, version: &str) -> Result<String> {
    let prefix = format!("## [{version}] - ");
    let mut sections = changelog.lines().filter(|line| line.starts_with(&prefix));
    let heading = sections
        .next()
        .ok_or("Changelog has no section for this release version")?;
    if sections.next().is_some() {
        return Err("Changelog contains duplicate release sections".into());
    }
    let date = heading
        .strip_prefix(&prefix)
        .ok_or("Invalid release heading")?;
    if date.len() != 10
        || !date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err("Release changelog date must use YYYY-MM-DD".into());
    }
    let body = changelog
        .lines()
        .skip_while(|line| *line != heading)
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    if !body
        .lines()
        .any(|line| line.starts_with("- ") || line.starts_with("* "))
    {
        return Err("Release changelog section has no change entries".into());
    }
    Ok(format!("{}\n", body.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_updates_local_versions_and_preserves_history() {
        let manifest = "[workspace.package]\nversion = \"1.2.2\"\n[workspace.dependencies]\nlocal = { version = \"=1.2.2\", path = \"crates/local\" }\nexternal = \"3.0\"\n";
        let source = "# Changelog\n\n## Unreleased\n\n### Fixed\n- Synthetic fix\n\n## [1.2.2] - 2026-01-01\n- Earlier change\n";
        let (manifest, changelog) = prepare_text(manifest, source, "1.2.3", "2026-01-02").unwrap();
        assert!(manifest.contains("version = \"1.2.3\""));
        assert!(manifest.contains("version = \"=1.2.3\""));
        assert!(manifest.contains("external = \"3.0\""));
        assert_eq!(
            notes(&changelog, "1.2.3").unwrap(),
            "### Fixed\n- Synthetic fix\n"
        );
        assert_eq!(notes(&changelog, "1.2.2").unwrap(), "- Earlier change\n");
        assert!(prepare_text(&manifest, &changelog, "1.2.3", "2026-01-02").is_err());
        assert!(prepare_text(&manifest, &changelog, "1.0.0", "2026-01-02").is_err());
        assert!(prepare_text(&manifest, &changelog, "2.0.0", "2026-01-02").is_err());
    }

    #[test]
    fn first_release_can_use_current_version_without_trailing_blank_lines() {
        let manifest = "[workspace.package]\nversion = \"0.1.0-alpha\"\n[workspace.dependencies]\n";
        let (_, changelog) = prepare_text(
            manifest,
            "# Changelog\n\n## Unreleased\n\n- Synthetic initial feature\n",
            "0.1.0-alpha",
            "2026-01-02",
        )
        .unwrap();
        assert!(changelog.ends_with("feature\n"));
        assert!(!changelog.ends_with("\n\n"));
        assert!(notes(&changelog, "0.1.0-alpha").is_ok());
    }

    #[test]
    fn notes_only_include_requested_release() {
        let source = "# Changelog\n\n## Unreleased\n- Future synthetic change\n\n## [1.2.3] - 2026-01-02\n\n### Fixed\n- Synthetic fix\n\n## [1.2.2] - 2026-01-01\n- Older synthetic fix\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "### Fixed\n- Synthetic fix\n"
        );
    }

    #[test]
    fn missing_empty_duplicate_or_undated_sections_fail() {
        for source in [
            "## Unreleased\n- Synthetic change",
            "## [1.2.3] - 2026-01-02\n### Added",
            "## [1.2.3] - 2026-01-02\n- First\n## [1.2.3] - 2026-01-03\n- Second",
            "## [1.2.3] - someday\n- Synthetic change",
        ] {
            assert!(notes(source, "1.2.3").is_err());
        }
    }
}
