use crate::{
    runner::{Outcome, RunReport, TaskRecord},
    workflow::StepKind,
};
use serde_json::json;
use std::{fmt::Write as _, fs, path::Path};

pub fn write(report: &RunReport, dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    for (name, text) in [
        ("timeline.jsonl", timeline(report)),
        ("junit.xml", junit(report)),
        ("run.json", summary_json(report)),
    ] {
        let path = dir.join(name);
        fs::write(&path, text)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }
    Ok(())
}

fn summary_json(report: &RunReport) -> String {
    let line = json!({
        "strict": report.strict,
        "succeeded": report.succeeded(),
        "elapsed_ms": millis(report.elapsed),
        "tools": report.tools,
    });
    format!("{line}\n")
}

fn timeline(report: &RunReport) -> String {
    let mut out = String::new();
    for record in &report.records {
        for step in &record.steps {
            let mut line = json!({
                "suite": record.suite,
                "task": record.task,
                "at_ms": millis(step.at),
                "took_ms": millis(step.took),
            });
            match &step.kind {
                StepKind::Call {
                    tool,
                    is_error,
                    code,
                } => {
                    line["kind"] = json!("call");
                    line["tool"] = json!(tool);
                    line["is_error"] = json!(is_error);
                    if let Some(code) = code {
                        line["code"] = json!(code);
                    }
                }
                StepKind::Expect { window, what, met } => {
                    line["kind"] = json!("expect");
                    line["window"] = json!(window);
                    line["what"] = json!(what);
                    line["met"] = json!(met);
                }
                StepKind::Note(text) => {
                    line["kind"] = json!("note");
                    line["text"] = json!(text);
                }
            }
            let _ = writeln!(out, "{line}");
        }
        let mut line = json!({
            "suite": record.suite,
            "task": record.task,
            "kind": "outcome",
            "outcome": outcome_name(&record.outcome),
            "took_ms": millis(record.elapsed),
        });
        match &record.outcome {
            Outcome::Failed(failure) => {
                line["failure"] = json!({"kind": failure.kind.name(), "message": failure.message});
            }
            Outcome::Blocked(missing) => {
                line["missing"] = json!(missing.iter().map(|c| c.name()).collect::<Vec<_>>());
            }
            Outcome::Passed => {}
        }
        let _ = writeln!(out, "{line}");
    }
    out
}

fn junit(report: &RunReport) -> String {
    let mut suites: Vec<(&str, Vec<&TaskRecord>)> = Vec::new();
    for record in &report.records {
        match suites.iter_mut().find(|(name, _)| *name == record.suite) {
            Some((_, records)) => records.push(record),
            None => suites.push((record.suite, vec![record])),
        }
    }
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<testsuites name=\"controlfreak-e2e\" tests=\"{}\" time=\"{:.3}\">",
        report.records.len(),
        report.elapsed.as_secs_f64()
    );
    for (name, records) in suites {
        let failures = records
            .iter()
            .filter(|r| match r.outcome {
                Outcome::Failed(_) => true,
                Outcome::Blocked(_) => report.strict,
                Outcome::Passed => false,
            })
            .count();
        let skipped = records
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Blocked(_)) && !report.strict)
            .count();
        let time: f64 = records.iter().map(|r| r.elapsed.as_secs_f64()).sum();
        let _ = writeln!(
            out,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\" skipped=\"{skipped}\" \
             time=\"{time:.3}\">",
            xml(name),
            records.len()
        );
        for record in records {
            let _ = write!(
                out,
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"",
                xml(name),
                xml(record.task),
                record.elapsed.as_secs_f64()
            );
            match &record.outcome {
                Outcome::Passed => out.push_str("/>\n"),
                Outcome::Failed(failure) => {
                    let _ = writeln!(
                        out,
                        ">\n      <failure type=\"{}\" message=\"{}\">{}</failure>\n    </testcase>",
                        failure.kind.name(),
                        xml(first_line(&failure.message)),
                        xml(&failure.message)
                    );
                }
                Outcome::Blocked(missing) => {
                    let message = format!("BLOCKED: missing {}", names(missing));
                    let element = if report.strict { "failure" } else { "skipped" };
                    let _ = writeln!(
                        out,
                        ">\n      <{element} message=\"{}\"/>\n    </testcase>",
                        xml(&message)
                    );
                }
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

pub fn task_line(record: &TaskRecord) -> String {
    let label = format!("{}/{}", record.suite, record.task);
    let seconds = record.elapsed.as_secs_f64();
    let calls = record
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Call { .. }))
        .count();
    match &record.outcome {
        Outcome::Passed => format!("  PASS     {label:<52} {seconds:>6.2}s  {calls:>3} calls"),
        Outcome::Failed(failure) => format!(
            "  FAIL     {label:<52} {seconds:>6.2}s  {calls:>3} calls\n           {}",
            first_line(&failure.to_string())
        ),
        Outcome::Blocked(missing) => format!(
            "  BLOCKED  {label:<52} {seconds:>6.2}s  missing {}",
            names(missing)
        ),
    }
}

pub fn summary(report: &RunReport) -> String {
    let passed = report.count(|o| matches!(o, Outcome::Passed));
    let failed = report.count(|o| matches!(o, Outcome::Failed(_)));
    let blocked = report.count(|o| matches!(o, Outcome::Blocked(_)));
    let verdict = if report.succeeded() { "ok" } else { "FAILED" };
    let strict = if report.strict { ", strict" } else { "" };
    format!(
        "e2e ({} tasks{strict}): {verdict}. {passed} passed, {failed} failed, {blocked} blocked \
         in {:.1}s",
        report.records.len(),
        report.elapsed.as_secs_f64()
    )
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed(_) => "failed",
        Outcome::Blocked(_) => "blocked",
    }
}

fn names(capabilities: &[crate::Capability]) -> String {
    capabilities
        .iter()
        .map(|c| c.name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && !matches!(c, '\n' | '\r' | '\t') => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Capability,
        workflow::{Failure, Step},
    };
    use std::time::Duration;

    fn record(task: &'static str, outcome: Outcome) -> TaskRecord {
        TaskRecord {
            suite: "smoke",
            task,
            outcome,
            elapsed: Duration::from_millis(1500),
            steps: vec![Step {
                at: Duration::from_millis(3),
                took: Duration::from_millis(120),
                kind: StepKind::Call {
                    tool: "click_mouse".into(),
                    is_error: false,
                    code: None,
                },
            }],
        }
    }

    fn report(strict: bool) -> RunReport {
        RunReport {
            strict,
            records: vec![
                record("a", Outcome::Passed),
                record(
                    "b",
                    Outcome::Failed(Failure::assertion("saw <x> & \"y\"\nline two")),
                ),
                record("c", Outcome::Blocked(vec![Capability::Ocr])),
            ],
            elapsed: Duration::from_secs(5),
            tools: Vec::new(),
        }
    }

    #[test]
    fn junit_escapes_text_and_counts_blocked_by_strictness() {
        let relaxed = junit(&report(false));
        assert!(
            relaxed.contains("failures=\"1\" skipped=\"1\""),
            "{relaxed}"
        );
        assert!(relaxed.contains("<skipped message=\"BLOCKED: missing ocr\"/>"));
        assert!(relaxed.contains("saw &lt;x&gt; &amp; &quot;y&quot;"));
        let strict = junit(&report(true));
        assert!(strict.contains("failures=\"2\" skipped=\"0\""), "{strict}");
        assert!(strict.contains("<failure message=\"BLOCKED: missing ocr\"/>"));
    }

    #[test]
    fn every_timeline_line_is_json_with_suite_task_and_kind() {
        let text = timeline(&report(true));
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON line"))
            .collect();
        assert_eq!(lines.len(), 6, "three steps and three outcomes");
        for line in &lines {
            assert_eq!(line["suite"], "smoke");
            assert!(
                line["task"].is_string() && line["kind"].is_string(),
                "{line}"
            );
        }
        assert_eq!(lines[0]["tool"], "click_mouse");
        assert_eq!(lines[0]["took_ms"], 120);
        assert_eq!(lines[3]["failure"]["kind"], "assertion");
        assert_eq!(lines[5]["missing"][0], "ocr");
    }

    #[test]
    fn the_summary_reflects_strictness() {
        assert!(summary(&report(false)).contains("FAILED. 1 passed, 1 failed, 1 blocked"));
        let only_blocked = RunReport {
            records: vec![record("c", Outcome::Blocked(vec![Capability::Ocr]))],
            ..report(false)
        };
        assert!(only_blocked.succeeded());
        assert!(
            !RunReport {
                strict: true,
                ..only_blocked
            }
            .succeeded()
        );
    }
}
