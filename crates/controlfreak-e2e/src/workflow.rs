use crate::{
    events::Event,
    host::Window,
    mcp::{McpClient, McpError, ToolResult},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    process::Output,
    thread,
    time::{Duration, Instant},
};

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Pr,
    Nightly,
}

impl Tier {
    pub(crate) const fn includes(self, task: Self) -> bool {
        match self {
            Self::Nightly => true,
            Self::Pr => matches!(task, Self::Pr),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Nightly => "nightly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Ocr,
    TaskbarForeground,
}

impl Capability {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::TaskbarForeground => "taskbar-foreground",
        }
    }

    fn probe(self, cx: &mut Ctx<'_>) -> Result<bool, Failure> {
        match self {
            Self::Ocr => {
                let result = cx.call(
                    "find_text_on_screen",
                    json!({"x": 0, "y": 0, "width": 64, "height": 64, "query": "probe"}),
                )?;
                Ok(!(result.is_error && result.error_reason() == Some("ocr_unavailable")))
            }
            Self::TaskbarForeground => Ok(cx.helper(&["--activate-taskbar"])?.status.success()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Assertion,
    Contract,
    Protocol,
    Timeout,
    Host,
}

impl FailureKind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Assertion => "assertion",
            Self::Contract => "contract",
            Self::Protocol => "protocol",
            Self::Timeout => "timeout",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn assertion(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Assertion, message)
    }

    pub(crate) fn host(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Host, message)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.name(), self.message)
    }
}

impl From<McpError> for Failure {
    fn from(error: McpError) -> Self {
        let kind = match error {
            McpError::Timeout { .. } => FailureKind::Timeout,
            McpError::Contract { .. } => FailureKind::Contract,
            McpError::Disconnected(_) | McpError::Protocol(_) => FailureKind::Protocol,
        };
        Self::new(kind, error.to_string())
    }
}

macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err($crate::workflow::Failure::assertion(format!($($message)+)));
        }
    };
}
pub(crate) use ensure;

pub type Body = fn(&mut Ctx<'_>) -> Result<(), Failure>;

#[derive(Debug, Clone, Copy)]
pub struct Task {
    pub name: &'static str,
    pub tier: Tier,
    pub scene: Option<&'static str>,
    pub requires: &'static [Capability],
    pub(crate) budget: Duration,
    pub(crate) body: Body,
}

pub const fn task(name: &'static str, body: Body) -> Task {
    Task {
        name,
        tier: Tier::Pr,
        scene: None,
        requires: &[],
        budget: DEFAULT_BUDGET,
        body,
    }
}

impl Task {
    #[must_use]
    pub const fn scene(mut self, scene: &'static str) -> Self {
        self.scene = Some(scene);
        self
    }

    #[must_use]
    pub const fn requires(mut self, capabilities: &'static [Capability]) -> Self {
        self.requires = capabilities;
        self
    }

    #[must_use]
    pub const fn nightly(mut self) -> Self {
        self.tier = Tier::Nightly;
        self
    }

    #[must_use]
    pub(crate) const fn budget_secs(mut self, seconds: u64) -> Self {
        self.budget = Duration::from_secs(seconds);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Suite {
    pub name: &'static str,
    pub about: &'static str,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub(crate) at: Duration,
    pub(crate) took: Duration,
    pub(crate) kind: StepKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    Call {
        tool: String,
        is_error: bool,
        code: Option<String>,
    },
    Expect {
        window: String,
        what: String,
        met: bool,
    },
    Note(String),
}

pub struct Ctx<'a> {
    client: &'a mut McpClient,
    windows: &'a [Window],
    helper: &'a mut dyn FnMut(&[&str]) -> Result<Output, String>,
    targets: HashMap<String, String>,
    started: Instant,
    pub(crate) steps: Vec<Step>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        client: &'a mut McpClient,
        windows: &'a [Window],
        helper: &'a mut dyn FnMut(&[&str]) -> Result<Output, String>,
    ) -> Self {
        Self {
            client,
            windows,
            helper,
            targets: HashMap::new(),
            started: Instant::now(),
            steps: Vec::new(),
        }
    }

    pub(crate) fn probe(&mut self, capability: Capability) -> Result<bool, Failure> {
        capability.probe(self)
    }

    fn record(&mut self, started: Instant, kind: StepKind) {
        self.steps.push(Step {
            at: started.duration_since(self.started),
            took: started.elapsed(),
            kind,
        });
    }

    pub(crate) fn note(&mut self, text: impl Into<String>) {
        let now = Instant::now();
        self.record(now, StepKind::Note(text.into()));
    }

    pub(crate) fn window(&self, key: &str) -> Result<&'a Window, Failure> {
        self.windows
            .iter()
            .find(|window| window.key == key)
            .ok_or_else(|| Failure::host(format!("the scene has no window {key:?}")))
    }

    pub(crate) fn center(&self, key: &str, region: &str) -> Result<(i32, i32), Failure> {
        self.window(key)?
            .regions
            .center(region)
            .ok_or_else(|| Failure::host(format!("window {key} reported no {region} region")))
    }

    pub fn target(&mut self, key: &str) -> Result<String, Failure> {
        if let Some(id) = self.targets.get(key) {
            return Ok(id.clone());
        }
        let title = self.window(key)?.title.clone();
        let found = self.ok(
            "wait_for_window",
            json!({"title_contains": title, "timeout_ms": 10_000}),
        )?;
        ensure!(
            found["matched"] == true,
            "window {key} was not found: {found}"
        );
        let id = found["window"]["id"]
            .as_str()
            .ok_or_else(|| Failure::assertion(format!("window {key} has no id: {found}")))?
            .to_owned();
        self.targets.insert(key.to_owned(), id.clone());
        Ok(id)
    }

    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, Failure> {
        let started = Instant::now();
        let result = self.client.call(tool, arguments);
        let kind = match &result {
            Ok(result) => StepKind::Call {
                tool: tool.to_owned(),
                is_error: result.is_error,
                code: result.error_code().map(str::to_owned),
            },
            Err(error) => StepKind::Call {
                tool: tool.to_owned(),
                is_error: true,
                code: Some(format!("harness:{error}")),
            },
        };
        self.record(started, kind);
        Ok(result?)
    }

    pub fn ok(&mut self, tool: &str, arguments: Value) -> Result<Value, Failure> {
        let result = self.call(tool, arguments)?;
        ensure!(!result.is_error, "{tool} failed: {}", result.structured);
        Ok(result.structured)
    }

    pub(crate) fn refused(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, Failure> {
        let result = self.call(tool, arguments)?;
        ensure!(
            result.is_error,
            "{tool} should have been refused: {}",
            result.structured
        );
        Ok(result)
    }

    pub(crate) fn begin(&mut self, target: &str) -> Result<(), Failure> {
        self.ok(
            "begin_control_session",
            json!({"target_ref": target, "expected_seconds": 60}),
        )
        .map(drop)
    }

    pub(crate) fn end(&mut self) -> Result<(), Failure> {
        self.ok("end_control_session", json!({})).map(drop)
    }

    pub(crate) fn focus(&mut self, target: &str) -> Result<(), Failure> {
        self.ok(
            "focus_window",
            json!({"window_id": target, "observation": {"mode": "none"}}),
        )
        .map(drop)
    }

    pub(crate) fn click(&mut self, target: &str, point: (i32, i32)) -> Result<ToolResult, Failure> {
        self.call(
            "click_mouse",
            json!({
                "target_ref": target, "x": point.0, "y": point.1,
                "observation": {"mode": "none"}
            }),
        )
    }

    pub(crate) fn type_text(&mut self, target: &str, text: &str) -> Result<(), Failure> {
        self.ok(
            "type_text",
            json!({"target_ref": target, "text": text, "observation": {"mode": "none"}}),
        )
        .map(drop)
    }

    pub(crate) fn press(&mut self, target: &str, keys: &[&str]) -> Result<(), Failure> {
        self.ok(
            "press_keys",
            json!({"target_ref": target, "keys": keys, "observation": {"mode": "none"}}),
        )
        .map(drop)
    }

    pub(crate) fn expect(
        &mut self,
        key: &str,
        what: &str,
        done: impl Fn(&[Event]) -> bool,
    ) -> Result<Vec<Event>, Failure> {
        let window = self.window(key)?;
        let started = Instant::now();
        let deadline = started + EVENT_TIMEOUT;
        loop {
            let events = window.events();
            let met = done(&events);
            if met || Instant::now() >= deadline {
                self.record(
                    started,
                    StepKind::Expect {
                        window: key.to_owned(),
                        what: what.to_owned(),
                        met,
                    },
                );
                if met {
                    return Ok(events);
                }
                let log = std::fs::read_to_string(&window.log).unwrap_or_default();
                return Err(Failure::new(
                    FailureKind::Timeout,
                    format!(
                        "window {key} never logged {what} within {EVENT_TIMEOUT:?}. Log:\n{log}"
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn helper(&mut self, args: &[&str]) -> Result<Output, Failure> {
        let started = Instant::now();
        let output = (self.helper)(args).map_err(Failure::host);
        self.record(
            started,
            StepKind::Note(format!("helper {}", args.join(" "))),
        );
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nightly_run_includes_pr_tasks_but_not_the_reverse() {
        assert!(Tier::Nightly.includes(Tier::Pr));
        assert!(Tier::Nightly.includes(Tier::Nightly));
        assert!(Tier::Pr.includes(Tier::Pr));
        assert!(!Tier::Pr.includes(Tier::Nightly));
    }

    #[test]
    fn the_builder_sets_every_field() {
        const CAPS: &[Capability] = &[Capability::Ocr];
        let built = task("t", |_| Ok(()))
            .scene("single")
            .requires(CAPS)
            .nightly()
            .budget_secs(5);
        assert_eq!(built.name, "t");
        assert_eq!(built.scene, Some("single"));
        assert_eq!(built.requires, CAPS);
        assert_eq!(built.tier, Tier::Nightly);
        assert_eq!(built.budget, Duration::from_secs(5));
    }

    #[test]
    fn protocol_errors_map_to_failure_kinds() {
        let timeout = Failure::from(McpError::Timeout {
            method: "tools/call".into(),
            after: Duration::from_secs(1),
        });
        assert_eq!(timeout.kind, FailureKind::Timeout);
        let contract = Failure::from(McpError::Contract {
            tool: "x".into(),
            detail: "y".into(),
        });
        assert_eq!(contract.kind, FailureKind::Contract);
        assert_eq!(
            Failure::from(McpError::Disconnected("z".into())).kind,
            FailureKind::Protocol
        );
    }
}
