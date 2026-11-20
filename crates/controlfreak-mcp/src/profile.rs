//! Which catalog rows a server serves, fixed at startup. `tools/list`, dispatch and the
//! `run_actions` step check all read the same [`Profile`], so a tool that is not listed cannot be
//! called by name or reached through a batch. A profile narrows the MCP tool surface; it is not a
//! sandbox for the applications that input reaches.

use std::fmt;

use serde_json::{Value, json};

use super::catalog::{self, CATALOG, Route, ToolSpec};

/// Longest part of a rejected `--tools` entry that an error repeats.
const MAX_ECHOED_NAME_CHARS: usize = 64;

/// The tools a server serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// Serves no tool that changes the desktop or reserves it.
    observe: bool,
    /// When set, only these tools, plus the ones every profile keeps. In catalog order.
    allowlist: Option<Vec<&'static str>>,
}

/// Why a profile configuration was refused at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// `--tools` names no tool.
    EmptyList,
    /// `--tools` has an empty entry, as in `a,,b` or a trailing comma.
    EmptyEntry,
    /// `--tools` names a tool that does not exist. Holds the name, shortened.
    UnknownTool(String),
    /// `--tools` names the same tool twice.
    Duplicate(&'static str),
    /// `--tools` names a tool that `--profile observe` does not serve.
    NotServedByObserve(&'static str),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyList => formatter.write_str("--tools needs at least one tool name"),
            Self::EmptyEntry => formatter
                .write_str("--tools has an empty entry; separate tool names with single commas"),
            Self::UnknownTool(name) => write!(
                formatter,
                "--tools names unknown tool '{name}'; valid names: {}",
                CATALOG
                    .iter()
                    .map(|spec| spec.name)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Duplicate(name) => write!(formatter, "--tools names '{name}' twice"),
            Self::NotServedByObserve(name) => write!(
                formatter,
                "--tools names '{name}', which --profile observe does not serve; remove it or drop --profile observe"
            ),
        }
    }
}

impl std::error::Error for ProfileError {}

impl Profile {
    /// Every tool.
    #[must_use]
    pub const fn full() -> Self {
        Self {
            observe: false,
            allowlist: None,
        }
    }

    /// Observation only: no control session, no focus or desktop switch and no input tool.
    #[must_use]
    pub const fn observe() -> Self {
        Self {
            observe: true,
            allowlist: None,
        }
    }

    /// Narrows this profile to the comma-separated tool names in `list`. Spaces around a name are
    /// ignored. The stop, status and end-session tools stay served whether or not they are listed.
    ///
    /// # Errors
    ///
    /// An empty list or entry, an unknown or repeated name, or a name this profile does not serve
    /// (with [`Profile::observe`]). Nothing is served in that case: the caller must not start.
    pub fn restrict_to(self, list: &str) -> Result<Self, ProfileError> {
        if list.trim().is_empty() {
            return Err(ProfileError::EmptyList);
        }
        let mut named: Vec<&'static ToolSpec> = Vec::new();
        for entry in list.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                return Err(ProfileError::EmptyEntry);
            }
            let Some(spec) = catalog::spec(entry) else {
                return Err(ProfileError::UnknownTool(
                    entry.chars().take(MAX_ECHOED_NAME_CHARS).collect(),
                ));
            };
            if named.iter().any(|other| other.name == spec.name) {
                return Err(ProfileError::Duplicate(spec.name));
            }
            if !self.serves(spec) {
                return Err(ProfileError::NotServedByObserve(spec.name));
            }
            named.push(spec);
        }
        let allowlist = CATALOG
            .iter()
            .filter(|spec| named.iter().any(|other| other.name == spec.name))
            .map(|spec| spec.name)
            .collect();
        Ok(Self {
            observe: self.observe,
            allowlist: Some(allowlist),
        })
    }

    /// Whether this profile serves every tool.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        !self.observe && self.allowlist.is_none()
    }

    /// Whether the tool of `spec` is served.
    pub(super) fn serves(&self, spec: &ToolSpec) -> bool {
        match spec.route {
            // The ways out of desktop work and the report of its state are never hidden.
            Route::Stop | Route::Status | Route::EndSession => true,
            Route::BeginSession | Route::Input(_) if self.observe => false,
            _ => self
                .allowlist
                .as_ref()
                .is_none_or(|allowlist| allowlist.contains(&spec.name)),
        }
    }

    /// The catalog row of `tool` if this profile serves it. A hidden tool is as unknown as a
    /// tool that does not exist.
    pub(super) fn spec(&self, tool: &str) -> Option<&'static ToolSpec> {
        catalog::spec(tool).filter(|spec| self.serves(spec))
    }

    /// The served catalog rows, in catalog order.
    pub(super) fn specs(&self) -> impl Iterator<Item = &'static ToolSpec> + '_ {
        CATALOG.iter().filter(|spec| self.serves(spec))
    }

    /// Whether a tool named `tool` is served.
    pub(super) fn serves_tool(&self, tool: &str) -> bool {
        self.spec(tool).is_some()
    }

    /// The profile as `get_server_status` and the startup log report it.
    pub(super) fn status_json(&self) -> Value {
        json!({
            "name": if self.observe { "observe" } else { "full" },
            "tool_allowlist": self.allowlist,
        })
    }
}
