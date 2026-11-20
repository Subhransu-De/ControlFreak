//! Tests that hold the published tool contract to the catalog, the docs and a snapshot.

use super::catalog::{CATALOG, TargetRef};
use super::schema::{full_tools, tools};
use serde_json::Value;
use std::fmt::Write as _;

/// The snapshot of every schema field. Regenerate it with
/// `CONTROLFREAK_UPDATE_SNAPSHOTS=1 cargo test -p controlfreak-mcp --lib schema_fields`
/// and review the diff: a changed line is a changed tool contract.
const SNAPSHOT: &str = include_str!("snapshots/tool_schema_fields.txt");
const SNAPSHOT_PATH: &str = "src/snapshots/tool_schema_fields.txt";

/// One line per field of `schema`: its path, whether its object requires it, and its default.
fn describe_fields(out: &mut String, prefix: &str, path: &str, schema: &Value) {
    let Some(object) = schema.as_object() else {
        return;
    };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();
        for name in names {
            let field = &properties[name.as_str()];
            let field_path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            let mut line = format!("{prefix} {field_path}");
            if required.contains(&name.as_str()) {
                line.push_str(" required");
            }
            if let Some(default) = field.get("default") {
                let _ = write!(line, " default={default}");
            }
            out.push_str(&line);
            out.push('\n');
            describe_fields(out, prefix, &field_path, field);
        }
    }
    if let Some(items) = object.get("items") {
        describe_fields(out, prefix, &format!("{path}[]"), items);
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        for (index, branch) in object
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let branch_path = if path.is_empty() {
                format!("{keyword}[{index}]")
            } else {
                format!("{path}.{keyword}[{index}]")
            };
            describe_fields(out, prefix, &branch_path, branch);
        }
    }
    if let Some(definitions) = object.get("$defs").and_then(Value::as_object) {
        let mut names: Vec<&String> = definitions.keys().collect();
        names.sort();
        for name in names {
            describe_fields(
                out,
                prefix,
                &format!("$defs.{name}"),
                &definitions[name.as_str()],
            );
        }
    }
}

fn schema_fields() -> String {
    let mut out = String::new();
    for tool in full_tools() {
        let input = Value::Object((*tool.input_schema).clone());
        describe_fields(&mut out, &format!("{} input", tool.name), "", &input);
        let output = Value::Object((*tool.output_schema.clone().unwrap_or_default()).clone());
        describe_fields(&mut out, &format!("{} output", tool.name), "", &output);
    }
    out
}

/// #5: a renamed, added or removed field, or a changed `required` or `default`, in any tool's
/// input or full output schema fails here until the snapshot is updated on purpose.
#[test]
fn schema_fields_match_the_snapshot() {
    let actual = schema_fields();
    if std::env::var_os("CONTROLFREAK_UPDATE_SNAPSHOTS").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH);
        std::fs::write(path, &actual).expect("write the schema snapshot");
        return;
    }
    let expected = SNAPSHOT.replace("\r\n", "\n");
    let removed: Vec<&str> = expected
        .lines()
        .filter(|line| !actual.lines().any(|other| other == *line))
        .collect();
    let added: Vec<&str> = actual
        .lines()
        .filter(|line| !expected.lines().any(|other| other == *line))
        .collect();
    assert!(
        removed.is_empty() && added.is_empty() && expected == actual,
        "the tool schemas changed; review and regenerate {SNAPSHOT_PATH}\nremoved:\n{}\nadded:\n{}",
        removed.join("\n"),
        added.join("\n"),
    );
}

/// The snapshot notices a change in a nested field and in a default, not only at the top level.
#[test]
fn the_schema_snapshot_sees_nested_fields_and_defaults() {
    let fields = schema_fields();
    for line in [
        "type_text input text required",
        "click_mouse input button default=\"left\"",
        "run_actions input steps[].action required",
        "type_text input observation.region.x required",
        "capture_display output $defs.screenshot.observation_id",
        "get_server_status output anyOf[0].dpi_awareness.oneOf[0].reason required",
    ] {
        assert!(fields.lines().any(|candidate| candidate == line), "{line}");
    }
}

/// #5: the tool table in docs/tools.md lists exactly the catalog's tools, with their kinds.
#[test]
fn the_documented_tool_table_matches_the_catalog() {
    let docs = include_str!("../../../docs/tools.md");
    let section = docs
        .split("\n## Tool list")
        .nth(1)
        .expect("docs/tools.md has a Tool list section");
    let section = section.split("\n## ").next().unwrap_or(section);
    let mut documented: Vec<(String, String)> = section
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            let name = cells.get(1)?.strip_prefix('`')?.strip_suffix('`')?;
            Some((name.to_owned(), (*cells.get(2)?).to_owned()))
        })
        .collect();
    let mut cataloged: Vec<(String, String)> = CATALOG
        .iter()
        .map(|spec| (spec.name.to_owned(), spec.documented_kind().to_owned()))
        .collect();
    documented.sort();
    cataloged.sort();
    assert_eq!(documented, cataloged);
    assert!(
        docs.contains(&format!("exposes {} MCP tools", CATALOG.len())),
        "docs/tools.md states the tool count"
    );
}

/// Only tools that read what the screen shows (pixels, or a window's controls) show the
/// indicator's reading state, and none of them takes input.
#[test]
fn only_tools_that_read_the_screen_show_the_reading_state() {
    let mut reading: Vec<&str> = CATALOG
        .iter()
        .filter(|spec| spec.reads_screen)
        .map(|spec| spec.name)
        .collect();
    reading.sort_unstable();
    assert_eq!(
        reading,
        [
            "capture_display",
            "capture_region",
            "capture_visual_baseline",
            "capture_window",
            "find_elements",
            "find_text_on_screen",
            "read_text_in_region",
            "wait_for_change_since",
            "wait_for_text",
            "wait_for_visual_change",
        ]
    );
    assert!(
        CATALOG
            .iter()
            .all(|spec| !(spec.reads_screen && spec.is_mutating()))
    );
}

/// The published annotations and `target_ref` follow each row.
#[test]
fn annotations_and_target_ref_follow_the_catalog() {
    for (spec, tool) in CATALOG.iter().zip(tools()) {
        let annotations = tool.annotations.expect("every tool is annotated");
        assert_eq!(tool.title.as_deref(), Some(spec.title), "{}", spec.name);
        assert_eq!(
            annotations.idempotent_hint,
            Some(spec.idempotent),
            "{}",
            spec.name
        );
        assert_eq!(
            annotations.open_world_hint,
            Some(spec.is_mutating()),
            "{}",
            spec.name
        );
        let required = tool.input_schema["required"].as_array().unwrap();
        let has_target = tool.input_schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_ref");
        let requires_target = required.iter().any(|name| name == "target_ref");
        match spec.target_ref {
            TargetRef::Required => assert!(has_target && requires_target, "{}", spec.name),
            TargetRef::Optional => assert!(has_target && !requires_target, "{}", spec.name),
            // The window tools declare target_ref themselves, as an optional alias of window_id.
            TargetRef::None => assert!(!requires_target, "{}", spec.name),
        }
    }
}
