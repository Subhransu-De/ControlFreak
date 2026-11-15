use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, BufReader, Read, Write},
    process::Child,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    Disconnected(String),
    Timeout { method: String, after: Duration },
    Protocol(String),
    Contract { tool: String, detail: String },
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected(detail) => write!(f, "server disconnected: {detail}"),
            Self::Timeout { method, after } => write!(f, "no reply to {method} within {after:?}"),
            Self::Protocol(detail) => write!(f, "protocol error: {detail}"),
            Self::Contract { tool, detail } => write!(f, "{tool} broke its contract: {detail}"),
        }
    }
}

impl std::error::Error for McpError {}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool: String,
    pub is_error: bool,
    pub structured: Value,
    pub content: Vec<Value>,
    pub elapsed: Duration,
}

impl ToolResult {
    pub fn error_code(&self) -> Option<&str> {
        self.structured["error"]["code"].as_str()
    }

    pub fn error_reason(&self) -> Option<&str> {
        self.structured["error"]["reason"].as_str()
    }

    pub fn error_message(&self) -> &str {
        self.structured["error"]["message"]
            .as_str()
            .unwrap_or_default()
    }

    pub fn image_data(&self) -> Option<&str> {
        self.content
            .iter()
            .find(|block| block["type"] == "image")
            .and_then(|block| block["data"].as_str())
    }
}

pub struct McpClient {
    writer: Option<Box<dyn Write + Send>>,
    incoming: Receiver<Value>,
    child: Option<Child>,
    next_id: u64,
    schemas: HashMap<String, jsonschema::Validator>,
    notifications: Vec<Value>,
    timeout: Duration,
    server_name: String,
}

impl McpClient {
    pub fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        child: Option<Child>,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let (sender, incoming) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str::<Value>(&line)
                    .unwrap_or_else(|_| json!({"__not_json": line}));
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            writer: Some(Box::new(writer)),
            incoming,
            child,
            next_id: 1,
            schemas: HashMap::new(),
            notifications: Vec::new(),
            timeout,
            server_name: String::new(),
        };
        let initialized = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "controlfreak-e2e", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        initialized["serverInfo"]["name"]
            .as_str()
            .ok_or_else(|| {
                McpError::Protocol(format!("initialize has no serverInfo: {initialized}"))
            })?
            .clone_into(&mut client.server_name);
        client.notify("notifications/initialized")?;
        client.load_schemas()?;
        Ok(client)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tool_names(&self) -> impl Iterator<Item = &str> {
        self.schemas.keys().map(String::as_str)
    }

    pub fn notifications(&self) -> &[Value] {
        &self.notifications
    }

    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, McpError> {
        let started = Instant::now();
        let mut params = json!({"name": tool});
        params["arguments"] = arguments;
        let result = self.request("tools/call", params)?;
        let elapsed = started.elapsed();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let structured = result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null);
        let content = result["content"].as_array().cloned().unwrap_or_default();
        self.check_contract(tool, is_error, &structured)?;
        Ok(ToolResult {
            tool: tool.to_owned(),
            is_error,
            structured,
            content,
            elapsed,
        })
    }

    fn check_contract(
        &self,
        tool: &str,
        is_error: bool,
        structured: &Value,
    ) -> Result<(), McpError> {
        let contract = |detail: String| McpError::Contract {
            tool: tool.to_owned(),
            detail,
        };
        if is_error {
            let error = &structured["error"];
            let well_formed = error["code"].is_string()
                && error["message"].is_string()
                && error["retryable"].is_boolean();
            return if well_formed {
                Ok(())
            } else {
                Err(contract(format!(
                    "error result lacks error.code, error.message or error.retryable: {structured}"
                )))
            };
        }
        let Some(validator) = self.schemas.get(tool) else {
            return Err(contract("the server did not list this tool".into()));
        };
        let problems: Vec<String> = validator
            .iter_errors(structured)
            .take(3)
            .map(|error| format!("{} at {}", error, error.instance_path()))
            .collect();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(contract(format!(
                "result does not match outputSchema: {}",
                problems.join("; ")
            )))
        }
    }

    fn load_schemas(&mut self) -> Result<(), McpError> {
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let listed = self.request("tools/list", params)?;
            let tools = listed["tools"]
                .as_array()
                .ok_or_else(|| McpError::Protocol(format!("tools/list has no tools: {listed}")))?;
            for tool in tools {
                let name = tool["name"]
                    .as_str()
                    .ok_or_else(|| McpError::Protocol(format!("a tool has no name: {tool}")))?;
                let schema = tool.get("outputSchema").cloned().unwrap_or(json!({}));
                let validator =
                    jsonschema::validator_for(&schema).map_err(|error| McpError::Contract {
                        tool: name.to_owned(),
                        detail: format!("outputSchema does not compile: {error}"),
                    })?;
                self.schemas.insert(name.to_owned(), validator);
            }
            match listed["nextCursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(()),
            }
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), McpError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| McpError::Disconnected("the client is closed".into()))?;
        writeln!(writer, "{message}")
            .and_then(|()| writer.flush())
            .map_err(|error| McpError::Disconnected(error.to_string()))
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method}))
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
        message["params"] = params;
        self.write(&message)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .incoming
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    RecvTimeoutError::Timeout => McpError::Timeout {
                        method: method.to_owned(),
                        after: self.timeout,
                    },
                    RecvTimeoutError::Disconnected => {
                        McpError::Disconnected(format!("stdout closed while waiting for {method}"))
                    }
                })?;
            if let Some(line) = message.get("__not_json") {
                return Err(McpError::Protocol(format!(
                    "stdout carried a non-JSON line: {line}"
                )));
            }
            if message.get("method").is_some() {
                self.notifications.push(message);
                continue;
            }
            if message["id"] != id {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(McpError::Protocol(format!(
                    "{method} was rejected: {error}"
                )));
            }
            return message.get("result").cloned().ok_or_else(|| {
                McpError::Protocol(format!("{method} reply has no result: {message}"))
            });
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        drop(self.writer.take());
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
