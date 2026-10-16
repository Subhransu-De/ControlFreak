use controlfreak_core::timing::StageTiming;
use rmcp::{ErrorData, model::CallToolResponse};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(super) struct Diagnostics {
    pub(super) instance_id: String,
    pub(super) started: Instant,
    pub(super) next_operation_id: AtomicU64,
    pub(super) completed: AtomicU64,
    pub(super) failed: AtomicU64,
    pub(super) last_completed: Mutex<Option<Instant>>,
    pub(super) recent_operations: Mutex<VecDeque<OperationRecord>>,
}

pub(super) struct OperationRecord {
    pub(super) operation_id: u64,
    pub(super) tool: String,
    pub(super) status: &'static str,
    pub(super) elapsed_ms: u64,
    pub(super) gap_ms: Option<u64>,
    pub(super) stages_ms: Value,
}

/// Renders stage totals as `{stage: milliseconds}` with microsecond precision.
pub(super) fn stages_json(stages: &[StageTiming]) -> Value {
    Value::Object(
        stages
            .iter()
            .map(|stage| {
                #[allow(clippy::cast_precision_loss)]
                let milliseconds = stage.micros as f64 / 1_000.0;
                (stage.stage.to_owned(), json!(milliseconds))
            })
            .collect(),
    )
}

impl Diagnostics {
    pub(super) fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        Self {
            instance_id: format!("cf-{}-{timestamp:x}", std::process::id()),
            started: Instant::now(),
            next_operation_id: AtomicU64::new(1),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_completed: Mutex::new(None),
            recent_operations: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    fn record(&self, record: OperationRecord) {
        let mut recent = self
            .recent_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recent.len() == 32 {
            recent.pop_front();
        }
        recent.push_back(record);
    }

    pub(super) fn complete(
        &self,
        tool_name: &str,
        operation_id: u64,
        started: Instant,
        gap_ms: Option<u64>,
        outcome: &Result<CallToolResponse, ErrorData>,
        stages_ms: Value,
    ) -> u64 {
        let failed = match outcome {
            Err(_) => true,
            Ok(CallToolResponse::Complete(result)) => result.is_error == Some(true),
            Ok(_) => false,
        };
        let status = match outcome {
            Ok(CallToolResponse::Complete(result)) => match result
                .structured_content
                .as_ref()
                .and_then(|value| value["status"].as_str())
            {
                Some("completed_unverified") => "completed_unverified",
                Some("partially_sent") => "partially_sent",
                Some("unknown") => "unknown",
                Some("not_started") => "not_started",
                _ if failed => "failed",
                _ => "completed",
            },
            _ if failed => "failed",
            _ => "completed",
        };
        if failed {
            self.failed.fetch_add(1, Ordering::Relaxed);
        } else {
            self.completed.fetch_add(1, Ordering::Relaxed);
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        *self
            .last_completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
        eprintln!(
            "{}",
            json!({
                "event": "tool_completed",
                "instance_id": self.instance_id,
                "operation_id": operation_id,
                "tool": tool_name,
                "status": status,
                "elapsed_ms": elapsed_ms,
                "gap_ms": gap_ms,
                "stages_ms": &stages_ms,
            })
        );
        self.record(OperationRecord {
            operation_id,
            tool: tool_name.to_owned(),
            status,
            elapsed_ms,
            gap_ms,
            stages_ms,
        });
        elapsed_ms
    }
}
