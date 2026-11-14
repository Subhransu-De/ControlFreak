//! Lossless edits of one MCP entry. Never include configuration contents in errors.
use std::collections::HashSet;

use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstNode, CstRootNode},
};
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table};

pub type Result<T> = std::result::Result<T, &'static str>;

/// Turns a message that includes a position into the error type.
///
/// The installer is a short-lived helper that reports one error and exits, so leaking one short
/// message is simpler than carrying an owned error through every call.
fn message(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The 1-based line and character column of a byte offset.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset.min(text.len())).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    (line, column)
}

pub fn equivalent(left: &str, right: &str, toml: bool) -> bool {
    let decode = |text: &str| -> Option<Value> {
        if toml {
            toml_edit::de::from_str(text).ok()
        } else {
            CstRootNode::parse(text, &ParseOptions::default())
                .ok()?
                .to_serde_value()
        }
    };
    match (decode(left), decode(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub enum Document {
    Toml(DocumentMut),
    Json(CstRootNode),
}

impl Document {
    pub fn parse(text: &str, toml: bool, comments: bool) -> Result<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if toml {
            return text
                .parse()
                .map(Self::Toml)
                .map_err(|error: toml_edit::TomlError| {
                    // The span is a byte range in the text. Report only where, never what.
                    let (line, column) = error
                        .span()
                        .map_or((1, 1), |span| line_and_column(text, span.start));
                    message(format!(
                        "Invalid TOML at line {line}, column {column}; repair the configuration and retry."
                    ))
                });
        }
        let options = ParseOptions {
            allow_comments: comments,
            allow_trailing_commas: comments,
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
            allow_bare_decimal_point_numbers: false,
            allow_non_finite_numbers: false,
            allow_extended_string_escapes: false,
        };
        let root = CstRootNode::parse(text, &options).map_err(|error| {
            message(format!(
                "Invalid JSON at line {}, column {}; repair the configuration and retry.",
                error.line_display(),
                error.column_display()
            ))
        })?;
        let object = root
            .object_value()
            .ok_or("Configuration must be an object.")?;
        check_unique(&object.into())?;
        Ok(Self::Json(root))
    }

    pub fn entry(&self, group: &str) -> Result<Option<String>> {
        match self {
            Self::Toml(doc) => {
                let Some(parent) = doc.get(group) else {
                    return Ok(None);
                };
                let parent = parent
                    .as_table_like()
                    .ok_or("MCP configuration must be a table.")?;
                // A standalone TOML document preserves all fields and comments of the entry.
                Ok(parent.get("controlfreak").map(|entry| {
                    let mut wrapper = DocumentMut::new();
                    wrapper["controlfreak"] = entry.clone();
                    wrapper.to_string()
                }))
            }
            Self::Json(root) => {
                let object = root
                    .object_value()
                    .ok_or("Configuration must be an object.")?;
                if object.get(group).is_none() {
                    return Ok(None);
                }
                let parent = object
                    .object_value(group)
                    .ok_or("MCP configuration must be an object.")?;
                Ok(parent
                    .get("controlfreak")
                    .and_then(|p| p.value())
                    .map(|v| v.to_string()))
            }
        }
    }

    pub fn set(&mut self, group: &str, entry: Option<&str>) -> Result<()> {
        match self {
            Self::Toml(doc) => {
                if doc.get(group).is_none() {
                    if entry.is_none() {
                        return Ok(());
                    }
                    doc[group] = Item::Table(Table::new());
                }
                let parent = doc
                    .get_mut(group)
                    .and_then(Item::as_table_like_mut)
                    .ok_or("MCP configuration must be a table.")?;
                if let Some(entry) = entry {
                    let value: DocumentMut =
                        entry.parse().map_err(|_| "Invalid saved TOML entry.")?;
                    parent.insert(
                        "controlfreak",
                        value
                            .get("controlfreak")
                            .ok_or("Missing saved entry.")?
                            .clone(),
                    );
                } else {
                    parent.remove("controlfreak");
                }
            }
            Self::Json(root) => {
                let object = root
                    .object_value()
                    .ok_or("Configuration must be an object.")?;
                if object.get(group).is_none() && entry.is_none() {
                    return Ok(());
                }
                let parent = object
                    .object_value_or_create(group)
                    .ok_or("MCP configuration must be an object.")?;
                if let Some(entry) = entry {
                    let value = CstRootNode::parse(entry, &ParseOptions::default())
                        .map_err(|_| "Invalid saved JSON entry.")?
                        .to_serde_value()
                        .ok_or("Invalid saved JSON value.")?;
                    let value = input(value);
                    if let Some(prop) = parent.get("controlfreak") {
                        prop.set_value(value);
                    } else {
                        parent.append("controlfreak", value);
                    }
                } else if let Some(prop) = parent.get("controlfreak") {
                    prop.remove();
                }
            }
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        match self {
            Self::Toml(doc) => doc.to_string(),
            Self::Json(doc) => doc.to_string(),
        }
    }
}

fn check_unique(node: &CstNode) -> Result<()> {
    if let Some(object) = node.as_object() {
        let mut names = HashSet::new();
        for prop in object.properties() {
            if !names.insert(prop.decoded_name().ok_or("Invalid JSON property.")?) {
                return Err("Duplicate JSON keys; resolve the ambiguity and retry.");
            }
            if let Some(value) = prop.value() {
                check_unique(&value)?;
            }
        }
    } else if let Some(array) = node.as_array() {
        for element in array.elements() {
            check_unique(&element)?;
        }
    }
    Ok(())
}

fn input(value: Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(v) => CstInputValue::Bool(v),
        Value::Number(v) => CstInputValue::Number(v.to_string()),
        Value::String(v) => CstInputValue::String(v),
        Value::Array(v) => CstInputValue::Array(v.into_iter().map(input).collect()),
        Value::Object(v) => {
            CstInputValue::Object(v.into_iter().map(|(k, v)| (k, input(v))).collect())
        }
    }
}

pub fn desired(executable: &str, toml: bool, opencode: bool) -> String {
    if toml {
        let mut doc = DocumentMut::new();
        doc["controlfreak"] = Item::Table(Table::new());
        doc["controlfreak"]["command"] = toml_edit::value(executable);
        doc.to_string()
    } else if opencode {
        serde_json::json!({"type":"local", "command":[executable], "enabled":true}).to_string()
    } else {
        serde_json::json!({"command":executable,"args":[]}).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::Document;

    fn error(text: &str, toml: bool) -> &'static str {
        Document::parse(text, toml, false)
            .err()
            .expect("invalid configuration")
    }

    #[test]
    fn parse_errors_give_line_and_column_but_never_content() {
        let json = error("{\n  \"token\": \"hunter2\",\n  oops\n}", false);
        assert!(json.contains("line 3, column 3"), "{json}");
        let toml = error("name = \"hunter2\"\nbroken line\n", true);
        assert!(toml.contains("line 2"), "{toml}");
        for message in [json, toml] {
            assert!(
                !message.contains("hunter2") && !message.contains("oops"),
                "{message}"
            );
        }
    }
}
