//! Parses tool arguments and turns a parse failure into a structured `invalid_arguments` error.
//!
//! The error names the JSON path of the field, what the field expects and the JSON kind it
//! received, and never the received value: arguments carry typed text, which must not reach an
//! error, a log or a transcript. Serde's own messages quote the value, so they are never passed
//! on; only the type-level parts of them (the expected type, the valid names) are.

use rmcp::{ErrorData, model::JsonObject};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use serde_path_to_error::Segment;
use std::fmt::Write as _;

/// Most valid field names an unknown-field error lists.
const MAX_VALID_FIELDS: usize = 32;

/// Parses `arguments` as `T`. A failure is an `invalid_params` error whose data, published as
/// `error.details`, has `path`, `problem` and, when known, `expected`, `received_kind` and (for an
/// unknown field) `valid_fields`.
pub(super) fn parse_arguments<T>(arguments: Option<JsonObject>) -> Result<T, ErrorData>
where
    T: DeserializeOwned,
{
    let arguments = Value::Object(arguments.unwrap_or_default());
    serde_path_to_error::deserialize(&arguments).map_err(|error| {
        let segments: Vec<&Segment> = error.path().iter().collect();
        ArgumentError::new(&segments, &error.inner().to_string(), &arguments).into_error_data()
    })
}

/// What was wrong with one argument.
#[derive(Debug, PartialEq, Eq)]
struct ArgumentError {
    path: String,
    problem: &'static str,
    expected: Option<String>,
    received_kind: Option<&'static str>,
    valid_fields: Option<Vec<String>>,
    /// Text written by this crate or core (a custom deserializer hint), never a received value.
    hint: Option<String>,
}

impl ArgumentError {
    /// Classifies serde's `message` for the value at `segments`. Only the shape of the message
    /// is read: the quoted received value in it is dropped.
    fn new(segments: &[&Segment], message: &str, arguments: &Value) -> Self {
        let path = path_text(segments);
        let found = value_at(arguments, segments);
        let mut error = Self {
            path,
            problem: "invalid_value",
            expected: None,
            received_kind: found.map(json_kind),
            valid_fields: None,
            hint: None,
        };
        if let Some(rest) = message.strip_prefix("invalid type: ") {
            error.problem = "invalid_type";
            error.set_mismatch(rest);
        } else if let Some(rest) = message.strip_prefix("invalid value: ") {
            error.set_mismatch(rest);
        } else if let Some(rest) = message.strip_prefix("invalid length ") {
            error.problem = "invalid_length";
            error.expected = rest
                .rsplit_once(", expected ")
                .map(|(_, expected)| expected_text(expected));
        } else if message.starts_with("unknown variant `") {
            error.problem = "unknown_variant";
            error.expected = Some(format!("one of: {}", listed_names(message).join(", ")));
            error.received_kind = Some("string");
        } else if let Some(rest) = message.strip_prefix("unknown field `") {
            // The unknown name is a key the client sent, not a value. The path ends with it when
            // serde found it while reading the object, and at the object when a type refused it
            // afterwards (a `run_actions` step whose action does not take it).
            error.problem = "unknown_field";
            if let Some((name, _)) = rest.split_once("`, ")
                && !matches!(segments.last(), Some(Segment::Map { key }) if key == name)
            {
                error.path = join(&error.path, name);
                error.received_kind = found.and_then(|step| step.get(name)).map(json_kind);
            }
            let mut names = listed_names(message);
            names.truncate(MAX_VALID_FIELDS);
            error.valid_fields = Some(names);
        } else if let Some(rest) = message.strip_prefix("missing field `") {
            error.problem = "missing_field";
            if let Some((name, _)) = rest.split_once('`') {
                error.path = join(&error.path, name);
            }
            error.received_kind = Some("missing");
        } else {
            // A custom deserializer message, such as the key-name hint. Those are written without
            // the received value; see `controlfreak_core::Key`.
            error.hint = Some(message.to_owned());
        }
        error
    }

    /// Reads `<unexpected>, expected <expected>` without keeping the unexpected value.
    fn set_mismatch(&mut self, rest: &str) {
        // The expected part comes from a type's own description and never contains this
        // separator, so the last one ends the received value even when the value contains it.
        if let Some((received, expected)) = rest.rsplit_once(", expected ") {
            self.expected = Some(expected_text(expected));
            if let Some(kind) = unexpected_kind(received) {
                self.received_kind = Some(kind);
            }
        }
    }

    /// One sentence, with no received value.
    fn message(&self) -> String {
        let at = if self.path.is_empty() {
            "arguments".to_owned()
        } else {
            self.path.clone()
        };
        match self.problem {
            "missing_field" => format!("{at} is required"),
            "unknown_field" => match self.valid_fields.as_deref() {
                Some([]) | None => format!("{at} is not a field; this takes no fields"),
                Some(names) => format!("{at} is not a field; valid fields: {}", names.join(", ")),
            },
            _ => {
                let mut text = at;
                if let Some(expected) = &self.expected {
                    let _ = write!(text, ": expected {expected}");
                    if let Some(kind) = self.received_kind {
                        let _ = write!(text, ", received {kind}");
                    }
                } else {
                    text.push_str(": invalid value");
                }
                if let Some(hint) = &self.hint {
                    let _ = write!(text, ": {hint}");
                }
                text
            }
        }
    }

    fn into_error_data(self) -> ErrorData {
        let reason = self.message();
        let mut details = json!({
            "path": self.path,
            "problem": self.problem,
            "reason": reason,
        });
        if let Some(expected) = self.expected {
            details["expected"] = json!(expected);
        }
        if let Some(kind) = self.received_kind {
            details["received_kind"] = json!(kind);
        }
        if let Some(fields) = self.valid_fields {
            details["valid_fields"] = json!(fields);
        }
        ErrorData::invalid_params(format!("invalid tool arguments: {reason}"), Some(details))
    }
}

/// `observation.region.x`, `steps[2].text`.
fn path_text(segments: &[&Segment]) -> String {
    let mut path = String::new();
    for segment in segments {
        match segment {
            Segment::Seq { index } => {
                let _ = write!(path, "[{index}]");
            }
            Segment::Map { key } => path = join(&path, key),
            Segment::Enum { .. } | Segment::Unknown => {}
        }
    }
    path
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

fn value_at<'a>(arguments: &'a Value, segments: &[&Segment]) -> Option<&'a Value> {
    segments
        .iter()
        .try_fold(arguments, |value, segment| match segment {
            Segment::Seq { index } => value.get(index),
            Segment::Map { key } => value.get(key.as_str()),
            Segment::Enum { .. } | Segment::Unknown => Some(value),
        })
}

const fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The JSON kind of serde's description of a received value, such as `integer 5` or `string`.
fn unexpected_kind(received: &str) -> Option<&'static str> {
    let kinds = [
        ("integer", "number"),
        ("floating point", "number"),
        ("boolean", "boolean"),
        ("string", "string"),
        ("character", "string"),
        ("unit value", "null"),
        ("null", "null"),
        ("sequence", "array"),
        ("map", "object"),
    ];
    kinds
        .iter()
        .find(|(prefix, _)| received.starts_with(prefix))
        .map(|(_, kind)| *kind)
}

/// A type's own description in JSON terms: `a string` is `string`, `u8` is an integer range.
fn expected_text(expected: &str) -> String {
    let range = |low: i128, high: i128| format!("integer from {low} to {high}");
    match expected {
        "a string" | "a borrowed string" | "a character" | "string or map" => "string".to_owned(),
        "a boolean" => "boolean".to_owned(),
        "a sequence" => "array".to_owned(),
        "u8" => range(0, u8::MAX.into()),
        "u16" => range(0, u16::MAX.into()),
        "u32" => range(0, u32::MAX.into()),
        "u64" => range(0, u64::MAX.into()),
        "i8" => range(i8::MIN.into(), i8::MAX.into()),
        "i16" => range(i16::MIN.into(), i16::MAX.into()),
        "i32" => range(i32::MIN.into(), i32::MAX.into()),
        "i64" => range(i64::MIN.into(), i64::MAX.into()),
        "f32" | "f64" => "number".to_owned(),
        _ if expected.starts_with("struct ")
            || expected.starts_with("internally tagged enum")
            || expected == "a map" =>
        {
            "object".to_owned()
        }
        _ if expected.starts_with("one of `") || expected.starts_with('`') => {
            format!("one of: {}", quoted_names(expected).join(", "))
        }
        _ => expected.to_owned(),
    }
}

/// The names serde lists after the last `expected` in an unknown-name message. The first quoted
/// name, the unknown one, is before it and is dropped.
fn listed_names(message: &str) -> Vec<String> {
    message
        .rsplit_once(", expected ")
        .map(|(_, list)| quoted_names(list))
        .unwrap_or_default()
}

fn quoted_names(list: &str) -> Vec<String> {
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requests::{
        ClickMouseInput, NoArguments, PressKeysInput, RunActionsInput, TypeTextInput,
    };

    const TARGET: &str = "target-00000000-0000-0000-0000-000000000001";

    fn refusal<T: DeserializeOwned + std::fmt::Debug>(arguments: Value) -> (String, Value) {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        let error = parse_arguments::<T>(Some(arguments)).unwrap_err();
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        (error.message.to_string(), error.data.unwrap())
    }

    #[test]
    fn a_wrong_type_names_the_path_and_both_kinds_without_the_value() {
        let (message, details) = refusal::<TypeTextInput>(json!({"target_ref": TARGET, "text": 5}));
        assert_eq!(details["path"], "text");
        assert_eq!(details["problem"], "invalid_type");
        assert_eq!(details["expected"], "string");
        assert_eq!(details["received_kind"], "number");
        assert_eq!(
            message,
            "invalid tool arguments: text: expected string, received number"
        );

        // Typed text in a field of the wrong type is never echoed.
        let secret = "hunter2, expected u32";
        let (message, details) =
            refusal::<ClickMouseInput>(json!({"target_ref": TARGET, "x": secret, "y": 1}));
        assert!(!message.contains("hunter2"), "{message}");
        assert!(!details.to_string().contains("hunter2"), "{details}");
        assert_eq!(details["path"], "x");
        assert_eq!(details["expected"], "integer from 0 to 4294967295");
        assert_eq!(details["received_kind"], "string");
    }

    #[test]
    fn nested_paths_and_ranges_are_reported() {
        let (_, details) = refusal::<TypeTextInput>(json!({
            "target_ref": TARGET, "text": "a",
            "observation": {"region": {"x": -1, "y": 0, "width": 1, "height": 1}}
        }));
        assert_eq!(details["path"], "observation.region.x");
        assert_eq!(details["problem"], "invalid_value");
        assert_eq!(details["received_kind"], "number");
        assert_eq!(details["expected"], "integer from 0 to 4294967295");

        let (_, details) = refusal::<ClickMouseInput>(
            json!({"target_ref": TARGET, "x": 1, "y": 1, "click_count": 1.5}),
        );
        assert_eq!(details["path"], "click_count");
        assert_eq!(details["expected"], "integer from 0 to 255");
        assert_eq!(details["received_kind"], "number");

        let (_, details) = refusal::<PressKeysInput>(json!({"target_ref": TARGET, "keys": "ctrl"}));
        assert_eq!(details["path"], "keys");
        assert_eq!(details["expected"], "array");
        assert_eq!(details["received_kind"], "string");
    }

    #[test]
    fn an_unknown_field_lists_the_valid_names() {
        let (message, details) =
            refusal::<TypeTextInput>(json!({"target_ref": TARGET, "text": "a", "txt": "a"}));
        assert_eq!(details["problem"], "unknown_field");
        assert_eq!(details["path"], "txt");
        assert_eq!(details["received_kind"], "string");
        assert_eq!(
            details["valid_fields"],
            json!(["target_ref", "text", "timeout_ms", "observation"])
        );
        assert!(message.contains("valid fields: target_ref, text, timeout_ms, observation"));

        let (_, details) = refusal::<TypeTextInput>(json!({
            "target_ref": TARGET, "text": "a", "observation": {"mode": "none", "zoom": 2}
        }));
        assert_eq!(details["path"], "observation.zoom");
        assert!(
            details["valid_fields"]
                .as_array()
                .unwrap()
                .contains(&json!("mode"))
        );

        let (message, details) = refusal::<NoArguments>(json!({"force": true}));
        assert_eq!(details["valid_fields"], json!([]));
        assert!(message.contains("takes no fields"), "{message}");
    }

    #[test]
    fn missing_fields_and_unknown_names_are_reported_without_the_value() {
        let (message, details) = refusal::<TypeTextInput>(json!({"target_ref": TARGET}));
        assert_eq!(details["path"], "text");
        assert_eq!(details["problem"], "missing_field");
        assert_eq!(details["received_kind"], "missing");
        assert_eq!(message, "invalid tool arguments: text is required");

        let (message, details) = refusal::<ClickMouseInput>(
            json!({"target_ref": TARGET, "x": 1, "y": 1, "button": "secret-button"}),
        );
        assert_eq!(details["path"], "button");
        assert_eq!(details["problem"], "unknown_variant");
        assert_eq!(details["expected"], "one of: left, right, middle");
        assert_eq!(details["received_kind"], "string");
        assert!(!message.contains("secret"), "{message}");
        assert!(!details.to_string().contains("secret"), "{details}");
    }

    #[test]
    fn batch_steps_are_reported_by_step() {
        let (message, details) = refusal::<RunActionsInput>(json!({
            "target_ref": TARGET, "steps": [{"action": "wait", "wait_ms": 1}, {"action": "type_text", "text": 5}]
        }));
        assert!(
            details["path"].as_str().unwrap().starts_with("steps[1]"),
            "{details}"
        );
        assert_eq!(details["expected"], "string");
        assert_eq!(details["received_kind"], "number");
        assert!(message.contains("steps[1]"), "{message}");

        let (_, details) = refusal::<RunActionsInput>(json!({
            "target_ref": TARGET, "steps": [{"action": "type_text"}]
        }));
        assert_eq!(details["path"], "steps[0].text");
        assert_eq!(details["problem"], "missing_field");

        let (message, details) = refusal::<RunActionsInput>(json!({
            "target_ref": TARGET, "steps": [{"action": "launch", "text": "private words"}]
        }));
        assert_eq!(details["problem"], "unknown_variant");
        assert!(
            details["expected"].as_str().unwrap().contains("type_text"),
            "{details}"
        );
        assert!(
            !message.contains("launch") && !message.contains("private"),
            "{message}"
        );
    }

    #[test]
    fn a_key_hint_keeps_its_suggestion_but_not_the_typed_name() {
        let (message, details) = refusal::<PressKeysInput>(
            json!({"target_ref": TARGET, "keys": ["ctrl", "my-secret-key"]}),
        );
        assert_eq!(details["path"], "keys[1]");
        assert_eq!(details["received_kind"], "string");
        assert!(!message.contains("my-secret-key"), "{message}");
        assert!(message.contains("unknown key"), "{message}");
    }
}
