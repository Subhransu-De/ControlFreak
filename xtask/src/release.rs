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
    Ok(format!("{}\n", unwrap_list_items(body.trim())))
}

/// GitHub release bodies keep single line breaks, so a hard-wrapped changelog entry would render
/// broken mid-sentence. Join only plain prose that continues a top-level list item; every other
/// line, whatever Markdown it is, is kept as written.
fn unwrap_list_items(body: &str) -> String {
    let source: Vec<&str> = body.lines().collect();
    let mut lines: Vec<String> = Vec::new();
    // The fence that opened the current code block, as (character, length).
    let mut open_fence: Option<(char, usize)> = None;
    // Whether the last kept line is a top-level item, or prose already joined onto one.
    let mut in_item = false;
    for (index, line) in source.iter().copied().enumerate() {
        if let Some(open) = open_fence {
            if closes_fence(line, open) {
                open_fence = None;
            }
            lines.push(line.to_owned());
            in_item = false;
            continue;
        }
        if let Some(fence) = opening_fence(line) {
            open_fence = Some(fence);
            lines.push(line.to_owned());
            in_item = false;
            continue;
        }
        // A pipe-less table header is only recognizable by the delimiter row after it.
        let table_header = source
            .get(index + 1)
            .is_some_and(|next| is_table_delimiter(next));
        match lines.last_mut() {
            Some(previous)
                if in_item
                    && !table_header
                    && !ends_with_hard_break(previous)
                    && is_item_prose(line) =>
            {
                let text = line.trim_start();
                previous.push(' ');
                previous.push_str(if ends_with_hard_break(text) {
                    text
                } else {
                    text.trim_end()
                });
            }
            _ => {
                in_item = line.starts_with("- ") || line.starts_with("* ");
                lines.push(line.to_owned());
            }
        }
    }
    lines.join("\n")
}

/// A line indented exactly two spaces that starts like ordinary text, not a new Markdown block.
fn is_item_prose(line: &str) -> bool {
    let Some(text) = line.strip_prefix("  ") else {
        return false;
    };
    // An ordered-list marker is digits, then `.` or `)`, then whitespace or the end of the line.
    let ordered = text.find(|c: char| !c.is_ascii_digit()).is_some_and(|end| {
        let rest = &text[end..];
        end > 0
            && (rest.starts_with('.') || rest.starts_with(')'))
            && rest[1..].chars().next().is_none_or(char::is_whitespace)
    });
    // `*` and `_` open inline emphasis unless followed by whitespace, which makes `*` a bullet.
    let emphasis = (text.starts_with('*') || text.starts_with('_'))
        && text[1..].chars().next().is_some_and(|c| !c.is_whitespace());
    !ordered
        && (emphasis
            || text
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || "`'\"([".contains(c)))
}

/// Markdown's explicit line break: two trailing spaces or a trailing backslash.
fn ends_with_hard_break(line: &str) -> bool {
    line.ends_with("  ") || line.ends_with('\\')
}

/// A GFM table delimiter row such as `--- | :---:` or `|---|---|`.
fn is_table_delimiter(line: &str) -> bool {
    let cells: Vec<&str> = line
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect();
    line.contains('-')
        && cells.iter().all(|cell| {
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            !dashes.is_empty() && dashes.chars().all(|c| c == '-')
        })
        && (cells.len() > 1 || line.contains('|'))
}

/// A backtick or tilde fence of at least three characters, as (character, length).
fn opening_fence(line: &str) -> Option<(char, usize)> {
    let line = line.trim_start();
    let marker = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = line.chars().take_while(|c| *c == marker).count();
    (length >= 3).then_some((marker, length))
}

/// A fence of the same character, at least as long as the opener, with nothing after it.
fn closes_fence(line: &str, (marker, length): (char, usize)) -> bool {
    let line = line.trim();
    line.chars().count() >= length && line.chars().all(|c| c == marker)
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
    fn wrapped_entries_become_single_lines() {
        let source = "## [1.2.3] - 2026-01-02\n\n### Fixed\n\n- First line of a fix\n  continues here\n  and ends here.\n- Second fix\n\n```text\n  indented code stays\n```\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "### Fixed\n\n- First line of a fix continues here and ends here.\n- Second fix\n\n```text\n  indented code stays\n```\n"
        );
    }

    #[test]
    fn nested_blocks_and_indented_fences_are_kept() {
        let source = "## [1.2.3] - 2026-01-02\n\n- Parent entry\n  wraps here.\n  1. First step\n  2) Second step\n  + Plus bullet\n  > Quote\n- Entry with code:\n\n  ```text\n  code line\n  ```\n  Text after the block\n  continues.\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "- Parent entry wraps here.\n  1. First step\n  2) Second step\n  + Plus bullet\n  > Quote\n- Entry with code:\n\n  ```text\n  code line\n  ```\n  Text after the block\n  continues.\n"
        );
    }

    #[test]
    fn tilde_fences_are_kept_and_only_close_their_own_kind() {
        let source = "## [1.2.3] - 2026-01-02\n\n- Entry with tilde code:\n\n  ~~~text\n  first line\n  ```not a closer\n  second line\n  ~~~\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "- Entry with tilde code:\n\n  ~~~text\n  first line\n  ```not a closer\n  second line\n  ~~~\n"
        );
    }

    #[test]
    fn indented_code_compact_tables_and_long_fences_are_kept() {
        let source = "## [1.2.3] - 2026-01-02\n\n- Entry with indented code:\n\n      let kept = true;\n      let also = kept;\n- Entry with a table:\n  |a|b|\n  |-|-|\n- Entry with a long fence:\n  ````markdown\n  ```\n  inner\n  ```\n  ````\n- Last entry\n  wraps.\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "- Entry with indented code:\n\n      let kept = true;\n      let also = kept;\n- Entry with a table:\n  |a|b|\n  |-|-|\n- Entry with a long fence:\n  ````markdown\n  ```\n  inner\n  ```\n  ````\n- Last entry wraps.\n"
        );
    }

    #[test]
    fn versions_emphasis_hard_breaks_and_pipeless_tables() {
        let source = "## [1.2.3] - 2026-01-02\n\n- Support for\n  2.0 is new and\n  **Windows 11** works, as does\n  _this_ line.\n- A line with a hard break  \n  stays on its own line.\n- Entry with a table:\n  Name | Value\n  --- | ---\n  a | b\n";
        assert_eq!(
            notes(source, "1.2.3").unwrap(),
            "- Support for 2.0 is new and **Windows 11** works, as does _this_ line.\n- A line with a hard break  \n  stays on its own line.\n- Entry with a table:\n  Name | Value\n  --- | ---\n  a | b\n"
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
