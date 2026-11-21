//! Accessibility queries and semantic actions: the controls a window exposes through UI
//! Automation.
//!
//! A query reads one window's subtree and returns, for each matching control, its role, name,
//! state, supported patterns and bounds in display-local physical pixels, plus a short-lived
//! opaque element reference. The reference scopes a later query (`within`) and names the element
//! of a semantic action (invoke, toggle, expand or collapse). An action is a desktop mutation and
//! runs inside a control session like pointer input.

use serde::{Deserialize, Serialize};

use crate::limits::{
    MAX_ELEMENT_DEPTH, MAX_ELEMENT_NAME_FILTER_CHARS, MAX_ELEMENT_TIMEOUT_MS, MAX_ELEMENTS,
    MIN_ELEMENT_TIMEOUT_MS,
};
use crate::{ActionObservation, ObservationOptions, PlatformError};

/// Declares the roles once, so the enum, its wire names and `ElementRole::ALL` cannot drift.
macro_rules! roles {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// The kind of control, from its UI Automation control type. `unknown` is a control type
        /// this server does not know.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ElementRole {
            $($variant,)*
        }

        impl ElementRole {
            /// Every role, in the order documented to clients.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The stable `snake_case` name used in schemas and results.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

roles! {
    AppBar => "app_bar",
    Button => "button",
    Calendar => "calendar",
    CheckBox => "check_box",
    ComboBox => "combo_box",
    Custom => "custom",
    DataGrid => "data_grid",
    DataItem => "data_item",
    Document => "document",
    Edit => "edit",
    Group => "group",
    Header => "header",
    HeaderItem => "header_item",
    Hyperlink => "hyperlink",
    Image => "image",
    List => "list",
    ListItem => "list_item",
    Menu => "menu",
    MenuBar => "menu_bar",
    MenuItem => "menu_item",
    Pane => "pane",
    ProgressBar => "progress_bar",
    RadioButton => "radio_button",
    ScrollBar => "scroll_bar",
    SemanticZoom => "semantic_zoom",
    Separator => "separator",
    Slider => "slider",
    Spinner => "spinner",
    SplitButton => "split_button",
    StatusBar => "status_bar",
    Tab => "tab",
    TabItem => "tab_item",
    Table => "table",
    Text => "text",
    Thumb => "thumb",
    TitleBar => "title_bar",
    ToolBar => "tool_bar",
    ToolTip => "tool_tip",
    Tree => "tree",
    TreeItem => "tree_item",
    Window => "window",
    Unknown => "unknown",
}

/// A UI Automation control pattern the element reports as available. A pattern says what the
/// control can do. The server performs only the patterns of [`ElementAction`]; every other
/// pattern only describes the control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementPattern {
    Invoke,
    Toggle,
    Value,
    RangeValue,
    Selection,
    SelectionItem,
    ExpandCollapse,
    Scroll,
    ScrollItem,
    Grid,
    GridItem,
    Table,
    TableItem,
    Text,
    Window,
    Transform,
    Dock,
    MultipleView,
    VirtualizedItem,
    ItemContainer,
}

impl ElementPattern {
    /// Every pattern, in the order results list them.
    pub const ALL: [Self; 20] = [
        Self::Invoke,
        Self::Toggle,
        Self::Value,
        Self::RangeValue,
        Self::Selection,
        Self::SelectionItem,
        Self::ExpandCollapse,
        Self::Scroll,
        Self::ScrollItem,
        Self::Grid,
        Self::GridItem,
        Self::Table,
        Self::TableItem,
        Self::Text,
        Self::Window,
        Self::Transform,
        Self::Dock,
        Self::MultipleView,
        Self::VirtualizedItem,
        Self::ItemContainer,
    ];
}

/// A semantic action on one element, performed through its UI Automation control pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementAction {
    /// `Invoke` of the invoke pattern, such as pressing a button.
    Invoke,
    /// `Toggle` of the toggle pattern, such as a check box.
    Toggle,
    /// `Expand` of the expand-collapse pattern, such as opening a combo box or a tree item.
    Expand,
    /// `Collapse` of the expand-collapse pattern.
    Collapse,
}

impl ElementAction {
    /// The pattern the element must support for this action.
    #[must_use]
    pub const fn pattern(self) -> ElementPattern {
        match self {
            Self::Invoke => ElementPattern::Invoke,
            Self::Toggle => ElementPattern::Toggle,
            Self::Expand | Self::Collapse => ElementPattern::ExpandCollapse,
        }
    }
}

/// One semantic action on an element that `find_elements` returned for the session's window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementActionRequest {
    /// The session's approved window; the element reference must have been issued for it.
    pub target_ref: String,
    pub element_ref: String,
    pub action: ElementAction,
    pub observation: ObservationOptions,
}

impl ElementActionRequest {
    /// Checks the references before the desktop is touched.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` naming an empty reference.
    pub fn validate(&self) -> Result<(), PlatformError> {
        for (argument, value) in [
            ("target_ref", &self.target_ref),
            ("element_ref", &self.element_ref),
        ] {
            if value.is_empty() {
                return Err(PlatformError::InvalidArgument {
                    argument: argument.to_owned(),
                    reason: "must not be empty".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// The evidence after a semantic action, as after keyboard input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementActionResult {
    pub observation: ActionObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindElementsRequest {
    /// The window whose subtree is read.
    pub target_ref: String,
    /// An element reference from an earlier result for the same `target_ref`: read only that
    /// element's subtree.
    pub within: Option<String>,
    /// Case-insensitive substring the element's name must contain.
    pub name: Option<String>,
    pub role: Option<ElementRole>,
    /// Levels below the root that are read; 0 reads the root only.
    pub max_depth: u32,
    /// Most elements returned.
    pub max_elements: u32,
    /// Time the provider may take, in milliseconds.
    pub timeout_ms: u32,
}

impl FindElementsRequest {
    /// Checks the limits and filters before any provider is asked.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` naming the first field out of range.
    pub fn validate(&self) -> Result<(), PlatformError> {
        let invalid = |argument: &str, reason: String| {
            Err(PlatformError::InvalidArgument {
                argument: argument.to_owned(),
                reason,
            })
        };
        if self.max_depth > MAX_ELEMENT_DEPTH {
            return invalid("max_depth", format!("must be at most {MAX_ELEMENT_DEPTH}"));
        }
        if !(1..=MAX_ELEMENTS).contains(&self.max_elements) {
            return invalid(
                "max_elements",
                format!("must be between 1 and {MAX_ELEMENTS}"),
            );
        }
        if !(MIN_ELEMENT_TIMEOUT_MS..=MAX_ELEMENT_TIMEOUT_MS).contains(&self.timeout_ms) {
            return invalid(
                "timeout_ms",
                format!("must be between {MIN_ELEMENT_TIMEOUT_MS} and {MAX_ELEMENT_TIMEOUT_MS}"),
            );
        }
        if let Some(name) = &self.name {
            if name.trim().is_empty() {
                return invalid("name", "must not be empty".to_owned());
            }
            if name.chars().count() > MAX_ELEMENT_NAME_FILTER_CHARS {
                return invalid(
                    "name",
                    format!("must be at most {MAX_ELEMENT_NAME_FILTER_CHARS} characters"),
                );
            }
        }
        if self.target_ref.is_empty() {
            return invalid("target_ref", "must not be empty".to_owned());
        }
        if self.within.as_deref().is_some_and(str::is_empty) {
            return invalid("within", "must not be empty".to_owned());
        }
        Ok(())
    }
}

/// An element's bounding rectangle in display-local physical pixels, the coordinate space of the
/// pointer tools. `x` and `y` can be negative, or reach past the display, for an element that is
/// partly outside the display that holds its centre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementBounds {
    pub display_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI Automation properties of one element, reported as they are"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementInfo {
    /// Opaque, short-lived and bound to the query's `target_ref`: the root of a later query
    /// (`within`), or the element of a semantic action.
    pub element_ref: String,
    pub role: ElementRole,
    /// The accessible name, empty when the element has none or is a password field.
    pub name: String,
    /// The name was cut to the result's name limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_truncated: bool,
    /// A password field. Its name is withheld and its value is never read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password: bool,
    /// Levels below the query root; the root is 0.
    pub depth: u32,
    pub enabled: bool,
    /// The element can take keyboard focus.
    pub focusable: bool,
    /// The element has keyboard focus now.
    pub focused: bool,
    /// The provider reports the element as not visible on screen, for example scrolled away.
    pub offscreen: bool,
    /// `None` when the element has no on-screen rectangle or its centre is on no display.
    pub bounds: Option<ElementBounds>,
    pub patterns: Vec<ElementPattern>,
}

/// Whether the query read everything in its scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementQueryStatus {
    /// Every element in scope was read.
    Complete,
    /// A limit cut the walk, or a provider failed for part of the tree. The elements returned
    /// are real, but others may exist.
    Partial,
}

/// A limit that stopped part of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementQueryLimit {
    /// Some elements had children below `max_depth`.
    MaxDepth,
    /// More elements matched than `max_elements`.
    MaxElements,
    /// The walk visited the server's fixed node limit.
    NodeLimit,
    /// `timeout_ms` passed, or a provider stopped answering.
    Timeout,
    /// The result reached the server's fixed size limit.
    ResponseBytes,
}

/// How much of the tree a query read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementQueryCounts {
    /// Elements read, matching or not.
    pub visited: u32,
    /// Parts of the tree whose provider failed or did not answer; their subtrees are missing.
    pub provider_errors: u32,
    /// Elements of another process inside the window, skipped with their subtrees.
    pub skipped_other_process: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindElementsResult {
    pub status: ElementQueryStatus,
    /// A limit in `limits_reached` cut the walk.
    pub truncated: bool,
    pub limits_reached: Vec<ElementQueryLimit>,
    pub counts: ElementQueryCounts,
    pub elements: Vec<ElementInfo>,
}

impl FindElementsResult {
    /// The status implied by the limits and provider errors.
    #[must_use]
    pub fn status_for(
        limits_reached: &[ElementQueryLimit],
        provider_errors: u32,
    ) -> ElementQueryStatus {
        if limits_reached.is_empty() && provider_errors == 0 {
            ElementQueryStatus::Complete
        } else {
            ElementQueryStatus::Partial
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> FindElementsRequest {
        FindElementsRequest {
            target_ref: "target-1".into(),
            within: None,
            name: Some("Approve".into()),
            role: Some(ElementRole::Button),
            max_depth: 8,
            max_elements: 10,
            timeout_ms: 1_000,
        }
    }

    #[test]
    fn requests_inside_the_limits_are_accepted() {
        request().validate().unwrap();
        let mut root_only = request();
        root_only.max_depth = 0;
        root_only.name = None;
        root_only.validate().unwrap();
    }

    #[test]
    fn each_out_of_range_field_is_named() {
        type Change = fn(&mut FindElementsRequest);
        let cases: [(&str, Change); 8] = [
            ("max_depth", |r| r.max_depth = MAX_ELEMENT_DEPTH + 1),
            ("max_elements", |r| r.max_elements = 0),
            ("max_elements", |r| r.max_elements = MAX_ELEMENTS + 1),
            ("timeout_ms", |r| r.timeout_ms = MIN_ELEMENT_TIMEOUT_MS - 1),
            ("timeout_ms", |r| r.timeout_ms = MAX_ELEMENT_TIMEOUT_MS + 1),
            ("name", |r| r.name = Some("  ".into())),
            ("name", |r| {
                r.name = Some("x".repeat(MAX_ELEMENT_NAME_FILTER_CHARS + 1));
            }),
            ("within", |r| r.within = Some(String::new())),
        ];
        for (argument, change) in cases {
            let mut request = request();
            change(&mut request);
            match request.validate() {
                Err(PlatformError::InvalidArgument {
                    argument: named, ..
                }) => {
                    assert_eq!(named, argument);
                }
                other => panic!("{argument}: {other:?}"),
            }
        }
    }

    #[test]
    fn an_action_names_its_pattern_and_needs_both_references() {
        assert_eq!(ElementAction::Toggle.pattern(), ElementPattern::Toggle);
        assert_eq!(
            ElementAction::Collapse.pattern(),
            ElementPattern::ExpandCollapse
        );
        let request = ElementActionRequest {
            target_ref: "target-1".into(),
            element_ref: "element-1".into(),
            action: ElementAction::Invoke,
            observation: ObservationOptions::default(),
        };
        request.validate().unwrap();
        let no_target = ElementActionRequest {
            target_ref: String::new(),
            ..request.clone()
        };
        let no_element = ElementActionRequest {
            element_ref: String::new(),
            ..request
        };
        for (argument, request) in [("target_ref", no_target), ("element_ref", no_element)] {
            assert!(matches!(
                request.validate(),
                Err(PlatformError::InvalidArgument { argument: named, .. }) if named == argument
            ));
        }
    }

    #[test]
    fn role_names_match_their_wire_form() {
        for role in ElementRole::ALL {
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                serde_json::Value::String(role.name().to_owned())
            );
        }
        assert_eq!(ElementRole::CheckBox.name(), "check_box");
    }

    #[test]
    fn status_is_partial_whenever_something_was_not_read() {
        assert_eq!(
            FindElementsResult::status_for(&[], 0),
            ElementQueryStatus::Complete
        );
        assert_eq!(
            FindElementsResult::status_for(&[ElementQueryLimit::MaxDepth], 0),
            ElementQueryStatus::Partial
        );
        assert_eq!(
            FindElementsResult::status_for(&[], 1),
            ElementQueryStatus::Partial
        );
    }
}
