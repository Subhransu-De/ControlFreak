use serde_json::Value;
use std::fmt::Write as _;

pub const MARKER: &str = "<!-- controlfreak-test-report -->";
pub const WORKSPACE_JOB: &str = "Tests";
pub const E2E_JOB: &str = "Live desktop tests";
const LIMIT: usize = 60_000;
const MAX_FAILURES: usize = 5;
const MAX_TEST_NAMES: usize = 20;
const STEP_TAIL: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoTests {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub failures: Vec<String>,
}

impl CargoTests {
    pub fn total(&self) -> u64 {
        self.passed + self.failed
    }
}

pub fn parse_cargo_test(log: &str) -> Option<CargoTests> {
    let mut tests = CargoTests::default();
    let mut seen = false;
    for raw in log.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("test result: ") {
            seen = true;
            for part in rest.split(';') {
                let words: Vec<&str> = part.split_whitespace().collect();
                let (count, label) = match words.as_slice() {
                    [_, count, label, ..] if part.contains('.') && count.parse::<u64>().is_ok() => {
                        (*count, *label)
                    }
                    [count, label, ..] => (*count, *label),
                    _ => continue,
                };
                let Ok(count) = count.parse::<u64>() else {
                    continue;
                };
                match label {
                    "passed" => tests.passed += count,
                    "failed" => tests.failed += count,
                    "ignored" => tests.ignored += count,
                    _ => {}
                }
            }
        } else if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
            && !tests.failures.iter().any(|known| known == name)
        {
            tests.failures.push(name.to_owned());
        }
    }
    seen.then_some(tests)
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct E2eRun {
    pub strict: bool,
    pub tools: Vec<String>,
    pub tasks: Vec<E2eTask>,
    pub calls: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct E2eTask {
    pub suite: String,
    pub task: String,
    pub outcome: String,
    pub took_ms: u64,
    pub failure_kind: String,
    pub failure_message: String,
    pub missing: Vec<String>,
    pub steps: Vec<String>,
}

impl E2eRun {
    fn count(&self, outcome: &str) -> usize {
        self.tasks.iter().filter(|t| t.outcome == outcome).count()
    }

    fn task(&self, suite: &str, task: &str) -> Option<&E2eTask> {
        self.tasks
            .iter()
            .find(|t| t.suite == suite && t.task == task)
    }
}

pub fn parse_e2e(timeline: &str, run_json: Option<&str>) -> E2eRun {
    let mut run = E2eRun::default();
    if let Some(summary) = run_json.and_then(|text| serde_json::from_str::<Value>(text).ok()) {
        run.strict = summary["strict"].as_bool().unwrap_or(false);
        run.tools = summary["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
    }
    let mut pending: Vec<(String, String, String)> = Vec::new();
    for line in timeline.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let suite = text(&value, "suite");
        let task = text(&value, "task");
        if value["kind"] == "outcome" {
            let steps = pending
                .iter()
                .filter(|(s, t, _)| *s == suite && *t == task)
                .map(|(_, _, line)| line.clone())
                .collect();
            pending.retain(|(s, t, _)| !(*s == suite && *t == task));
            run.tasks.push(outcome(&value, suite, task, steps));
            continue;
        }
        if value["kind"] == "call"
            && !value["code"]
                .as_str()
                .is_some_and(|code| code.starts_with("harness:"))
        {
            run.calls
                .push((text(&value, "tool"), value["took_ms"].as_u64().unwrap_or(0)));
        }
        if let Some(line) = step_line(&value) {
            pending.push((suite, task, line));
        }
    }
    run
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

fn step_line(value: &Value) -> Option<String> {
    let elapsed = value["took_ms"].as_u64().unwrap_or(0);
    let at = seconds(value["at_ms"].as_u64().unwrap_or(0));
    match value["kind"].as_str()? {
        "call" => {
            let status = if value["is_error"] == true {
                value["code"].as_str().unwrap_or("error").to_owned()
            } else {
                "ok".to_owned()
            };
            Some(format!(
                "{at}  {:<24} {status:<20} {elapsed:>5} ms",
                text(value, "tool")
            ))
        }
        "expect" => {
            let met = if value["met"] == true {
                "met"
            } else {
                "NOT MET"
            };
            Some(format!(
                "{at}  expect {} \"{}\"  {met}  {elapsed} ms",
                text(value, "window"),
                text(value, "what")
            ))
        }
        "note" => Some(format!("{at}  {}", text(value, "text"))),
        _ => None,
    }
}

fn outcome(value: &Value, suite: String, task: String, steps: Vec<String>) -> E2eTask {
    E2eTask {
        outcome: value["outcome"].as_str().unwrap_or("failed").to_owned(),
        took_ms: value["took_ms"].as_u64().unwrap_or(0),
        failure_kind: text(&value["failure"], "kind"),
        failure_message: text(&value["failure"], "message"),
        missing: value["missing"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        steps,
        suite,
        task,
    }
}

fn seconds(ms: u64) -> String {
    format!("+{}.{:03}s", ms / 1000, ms % 1000)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub conclusion: Option<String>,
    pub seconds: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct Results {
    pub commit: String,
    pub workspace: Option<CargoTests>,
    pub e2e: Option<E2eRun>,
    pub jobs: Vec<Job>,
}

impl Results {
    fn job(&self, name: &str) -> Option<&Job> {
        self.jobs.iter().find(|job| job.name == name)
    }
}

pub struct Report<'a> {
    pub current: &'a Results,
    pub baseline: Option<&'a Results>,
    pub run_url: &'a str,
}

pub fn render_running(commit: &str, run_url: &str) -> String {
    format!(
        "{MARKER}\n### ⏳ Tests running\n\nThe workspace tests and the live e2e suites are running for `{}`. This comment updates when the run finishes.\n\n<sub>[CI run]({run_url})</sub>\n",
        short(commit)
    )
}

pub fn render(report: &Report<'_>) -> String {
    let current = report.current;
    let e2e = current.e2e.as_ref();
    let mut sections: Vec<(u8, String)> = vec![(0, format!("{MARKER}\n{}\n", title(current)))];
    sections.extend(alerts(current));
    sections.push((0, lanes(report)));
    if let Some(e2e) = e2e
        && let Some(task) = e2e.tasks.iter().find(|t| t.outcome == "failed")
        && !task.steps.is_empty()
    {
        let tail: Vec<&str> = task
            .steps
            .iter()
            .rev()
            .take(STEP_TAIL)
            .rev()
            .map(String::as_str)
            .collect();
        sections.push((
            2,
            details(
                "Last steps before the failure",
                &fenced(&tail.join("\n"), STEP_TAIL),
            ),
        ));
    }
    if let Some(e2e) = e2e.filter(|e| !e.tasks.is_empty()) {
        let used = used_tools(e2e).len();
        sections.push((
            3,
            details("Slowest e2e tasks", &slowest(e2e, report.baseline)),
        ));
        sections.push((
            4,
            details(
                &format!(
                    "Tool coverage: {used} of {} tools called",
                    e2e.tools.len().max(used)
                ),
                &coverage(e2e, report.baseline),
            ),
        ));
        sections.push((5, details("Run timeline", &gantt(e2e))));
    }
    let baseline = report.baseline.map_or_else(
        || "no baseline from `main` yet".to_owned(),
        |b| format!("baseline `main@{}`", short(&b.commit)),
    );
    let footer = format!(
        "<sub>Commit `{}` · [CI run]({}) · {baseline}</sub>\n",
        short(&current.commit),
        report.run_url
    );
    fit(sections, &footer)
}

fn title(current: &Results) -> String {
    let workspace = current.workspace.as_ref();
    let e2e = current.e2e.as_ref();
    let workspace_failed = workspace.map_or(0, |w| w.failed);
    let e2e_failed = e2e.map_or(0, |e| e.count("failed"));
    let e2e_blocked = e2e.map_or(0, |e| e.count("blocked"));
    let strict = e2e.is_some_and(|e| e.strict);
    let job_failed = |name: &str| {
        current
            .job(name)
            .and_then(|job| job.conclusion.as_deref())
            .is_some_and(|c| c != "success" && c != "skipped")
    };
    let missing_workspace = workspace.is_none() && job_failed(WORKSPACE_JOB);
    let missing_e2e = e2e.is_none() && job_failed(E2E_JOB);
    let passed = workspace.map_or(0, |w| w.passed)
        + u64::try_from(e2e.map_or(0, |e| e.count("passed"))).unwrap_or(0);
    let failing = workspace_failed + u64::try_from(e2e_failed).unwrap_or(0);
    let wall = [WORKSPACE_JOB, E2E_JOB]
        .iter()
        .filter_map(|name| current.job(name).and_then(|job| job.seconds))
        .max();
    let wall = wall
        .map(|s| format!(" · {}", duration(s)))
        .unwrap_or_default();

    if failing > 0 {
        format!("### ❌ {failing} failing · {passed} passed{wall}")
    } else if missing_workspace || missing_e2e {
        "### ❌ A test job failed before it produced results".to_owned()
    } else if e2e_blocked > 0 && strict {
        format!("### ⛔ {e2e_blocked} e2e tasks blocked · `--strict` counts them as failures")
    } else {
        format!("### ✅ All tests passed · {passed} tests{wall}")
    }
}

fn alerts(current: &Results) -> Vec<(u8, String)> {
    let workspace = current.workspace.as_ref();
    let e2e = current.e2e.as_ref();
    let mut sections = Vec::new();
    if let Some(workspace) = workspace.filter(|w| !w.failures.is_empty()) {
        let mut text = String::from("> [!CAUTION]\n> **Workspace tests failing:**\n>\n");
        for name in workspace.failures.iter().take(MAX_TEST_NAMES) {
            let _ = writeln!(text, "> - {}", code(name));
        }
        if workspace.failures.len() > MAX_TEST_NAMES {
            let _ = writeln!(
                text,
                "> - and {} more",
                workspace.failures.len() - MAX_TEST_NAMES
            );
        }
        sections.push((0, text));
    }
    if let Some(e2e) = e2e {
        for task in e2e
            .tasks
            .iter()
            .filter(|t| t.outcome == "failed")
            .take(MAX_FAILURES)
        {
            let mut text = format!(
                "> [!CAUTION]\n> **{}/{}** failed after {} ({})\n>\n",
                plain(&task.suite),
                code(&task.task),
                secs(task.took_ms),
                plain(&task.failure_kind)
            );
            text.push_str(&quoted_block(&task.failure_message, 12));
            let _ = writeln!(
                text,
                ">\n> Reproduce on a disposable desktop: `cargo xtask e2e --suite {} --strict`",
                ident(&task.suite)
            );
            sections.push((1, text));
        }
        let blocked: Vec<&E2eTask> = e2e
            .tasks
            .iter()
            .filter(|t| t.outcome == "blocked")
            .collect();
        if !blocked.is_empty() {
            let mut text = format!(
                "> [!WARNING]\n> The runner lacks a capability, so {} task{} could not run:\n>\n",
                blocked.len(),
                if blocked.len() == 1 { "" } else { "s" }
            );
            for task in &blocked {
                let missing: Vec<String> = task.missing.iter().map(|m| code(m)).collect();
                let _ = writeln!(
                    text,
                    "> - {}/{} (needs {})",
                    plain(&task.suite),
                    code(&task.task),
                    missing.join(", ")
                );
            }
            text.push_str(">\n> This comes from the runner image, not from this PR.\n");
            sections.push((1, text));
        }
    }
    sections
}

fn fit(mut sections: Vec<(u8, String)>, footer: &str) -> String {
    loop {
        let body: String = sections
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if body.len() + footer.len() < LIMIT {
            return format!("{body}\n{footer}");
        }
        let Some(lowest) = sections.iter().map(|(p, _)| *p).max() else {
            return format!("{MARKER}\n{footer}");
        };
        if lowest == 0 {
            let mut text = body;
            let mut cut = LIMIT.saturating_sub(footer.len() + 64);
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            return format!("{text}\n\n…trimmed to fit GitHub's comment limit.\n\n{footer}");
        }
        if let Some(index) = sections.iter().rposition(|(p, _)| *p == lowest) {
            sections.remove(index);
        }
    }
}

fn lanes(report: &Report<'_>) -> String {
    let current = report.current;
    let baseline = report.baseline;
    let mut out =
        String::from("| Lane | Result | Δ tests | Time | Δ time |\n|---|---|---:|---:|---:|\n");
    let workspace_result = match &current.workspace {
        Some(w) if w.failed > 0 => format!("❌ {} failed · {} passed", w.failed, w.passed),
        Some(w) => format!("✅ {} passed · {} ignored", w.passed, w.ignored),
        None => job_status(current.job(WORKSPACE_JOB)),
    };
    let workspace_delta = delta_count(
        current.workspace.as_ref().map(CargoTests::total),
        baseline
            .and_then(|b| b.workspace.as_ref())
            .map(CargoTests::total),
    );
    let _ = writeln!(
        out,
        "| Workspace | {workspace_result} | {workspace_delta} | {} | {} |",
        job_time(current, WORKSPACE_JOB),
        job_delta(current, baseline, WORKSPACE_JOB)
    );
    let e2e_result = match &current.e2e {
        Some(e) => {
            let (passed, failed, blocked) =
                (e.count("passed"), e.count("failed"), e.count("blocked"));
            let mut parts = Vec::new();
            if failed > 0 {
                parts.push(format!("❌ {failed} failed"));
            }
            if blocked > 0 {
                parts.push(format!("⛔ {blocked} blocked"));
            }
            if parts.is_empty() {
                format!("✅ {passed} passed")
            } else {
                parts.push(format!("{passed} passed"));
                parts.join(" · ")
            }
        }
        None => job_status(current.job(E2E_JOB)),
    };
    let e2e_delta = delta_count(
        current.e2e.as_ref().map(|e| e.tasks.len() as u64),
        baseline
            .and_then(|b| b.e2e.as_ref())
            .map(|e| e.tasks.len() as u64),
    );
    let _ = writeln!(
        out,
        "| Live e2e (PR tier) | {e2e_result} | {e2e_delta} | {} | {} |",
        job_time(current, E2E_JOB),
        job_delta(current, baseline, E2E_JOB)
    );
    out
}

fn job_status(job: Option<&Job>) -> String {
    match job.and_then(|j| j.conclusion.as_deref()) {
        Some("success") => "✅ no results uploaded".to_owned(),
        Some("skipped") => "skipped".to_owned(),
        Some(other) => format!("❌ job {}", plain(other)),
        None => "⏳ running".to_owned(),
    }
}

fn job_time(results: &Results, name: &str) -> String {
    results
        .job(name)
        .and_then(|job| job.seconds)
        .map_or_else(|| "–".to_owned(), duration)
}

fn job_delta(current: &Results, baseline: Option<&Results>, name: &str) -> String {
    let now = current.job(name).and_then(|job| job.seconds);
    let then = baseline
        .and_then(|b| b.job(name))
        .and_then(|job| job.seconds);
    match (now, then) {
        (Some(now), Some(then)) => signed_seconds(i128::from(now) - i128::from(then)),
        (Some(_), None) => "new".to_owned(),
        _ => "–".to_owned(),
    }
}

fn delta_count(now: Option<u64>, then: Option<u64>) -> String {
    match (now, then) {
        (Some(now), Some(then)) => {
            let diff = i128::from(now) - i128::from(then);
            if diff == 0 {
                "±0".to_owned()
            } else {
                format!("{diff:+}")
            }
        }
        (Some(_), None) => "new".to_owned(),
        _ => "–".to_owned(),
    }
}

fn signed_seconds(diff: i128) -> String {
    match diff {
        0 => "±0s".to_owned(),
        d if d > 0 => format!("+{d}s"),
        d => format!("−{}s", -d),
    }
}

fn slowest(e2e: &E2eRun, baseline: Option<&Results>) -> String {
    let mut tasks: Vec<&E2eTask> = e2e.tasks.iter().collect();
    tasks.sort_by_key(|t| std::cmp::Reverse(t.took_ms));
    let mut out = String::from("| Task | Time | Δ vs `main` |\n|---|---:|---:|\n");
    for task in tasks.iter().take(5) {
        let before = baseline
            .and_then(|b| b.e2e.as_ref())
            .and_then(|b| b.task(&task.suite, &task.task))
            .map(|t| t.took_ms);
        let delta = before.map_or_else(
            || "new".to_owned(),
            |before| {
                let diff = i128::from(task.took_ms) - i128::from(before);
                if diff == 0 {
                    "±0.00s".to_owned()
                } else {
                    let sign = if diff > 0 { "+" } else { "−" };
                    format!(
                        "{sign}{}",
                        secs(u64::try_from(diff.unsigned_abs()).unwrap_or(u64::MAX))
                    )
                }
            },
        );
        let _ = writeln!(
            out,
            "| {}/{} | {} | {delta} |",
            plain(&task.suite),
            code(&task.task),
            secs(task.took_ms)
        );
    }
    out
}

fn used_tools(e2e: &E2eRun) -> Vec<String> {
    let mut tools: Vec<String> = e2e.calls.iter().map(|(tool, _)| tool.clone()).collect();
    tools.sort();
    tools.dedup();
    tools
}

fn latencies(e2e: &E2eRun, tool: &str) -> Vec<u64> {
    let mut values: Vec<u64> = e2e
        .calls
        .iter()
        .filter(|(t, _)| t == tool)
        .map(|(_, ms)| *ms)
        .collect();
    values.sort_unstable();
    values
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn coverage(e2e: &E2eRun, baseline: Option<&Results>) -> String {
    let mut used: Vec<(String, Vec<u64>)> = used_tools(e2e)
        .into_iter()
        .map(|tool| {
            let values = latencies(e2e, &tool);
            (tool, values)
        })
        .collect();
    used.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    let mut out =
        String::from("| Tool | Calls | p50 | p95 | Δ p95 vs `main` |\n|---|---:|---:|---:|---:|\n");
    for (tool, values) in &used {
        let p95 = percentile(values, 95);
        let before = baseline
            .and_then(|b| b.e2e.as_ref())
            .map(|b| latencies(b, tool));
        let delta = match before {
            Some(before) if !before.is_empty() => {
                let diff = i128::from(p95) - i128::from(percentile(&before, 95));
                match diff {
                    0 => "±0 ms".to_owned(),
                    d if d > 0 => format!("+{d} ms"),
                    d => format!("−{} ms", -d),
                }
            }
            _ => "new".to_owned(),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} ms | {p95} ms | {delta} |",
            code(tool),
            values.len(),
            percentile(values, 50)
        );
    }
    let names: Vec<String> = used.iter().map(|(t, _)| t.clone()).collect();
    let never: Vec<String> = e2e
        .tools
        .iter()
        .filter(|tool| !names.contains(tool))
        .map(|tool| code(tool))
        .collect();
    if !never.is_empty() {
        let _ = write!(
            out,
            "\nNever called ({}): {}\n",
            never.len(),
            never.join(" ")
        );
    }
    out
}

fn gantt(e2e: &E2eRun) -> String {
    let mut out = String::from("```mermaid\ngantt\n  dateFormat x\n  axisFormat %M:%S\n");
    let mut start = 0u64;
    let mut section = String::new();
    for task in &e2e.tasks {
        if task.suite != section {
            section.clone_from(&task.suite);
            let _ = writeln!(out, "  section {}", ident(&task.suite));
        }
        let end = start + task.took_ms.max(1);
        let tag = match task.outcome.as_str() {
            "failed" => "crit, ",
            "blocked" => "active, ",
            _ => "",
        };
        let _ = writeln!(out, "  {} :{tag}{start}, {end}", ident(&task.task));
        start = end;
    }
    out.push_str("```\n");
    out
}

fn details(summary: &str, body: &str) -> String {
    format!("<details>\n<summary>{summary}</summary>\n\n{body}\n</details>\n")
}

fn fenced(text: &str, max_lines: usize) -> String {
    let lines: Vec<String> = text
        .lines()
        .take(max_lines)
        .map(|line| line.replace("~~~", "~ ~ ~"))
        .collect();
    format!("~~~text\n{}\n~~~", lines.join("\n"))
}

fn quoted_block(text: &str, max_lines: usize) -> String {
    let mut out = String::new();
    for line in fenced(text, max_lines).lines() {
        let _ = writeln!(out, "> {line}");
    }
    out
}

fn ident(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(80)
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_owned()
    } else {
        cleaned
    }
}

fn code(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| *c != '`' && !c.is_control())
        .take(200)
        .collect();
    format!("`{}`", cleaned.replace('|', "\\|"))
}

fn plain(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().take(200) {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '#' | '!' | '~' => {
                out.push('\\');
                out.push(c);
            }
            '@' => out.push_str("@\u{200b}"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn short(commit: &str) -> String {
    ident(&commit.chars().take(7).collect::<String>())
}

fn secs(ms: u64) -> String {
    format!("{}.{:02}s", ms / 1000, (ms % 1000) / 10)
}

fn duration(seconds: u64) -> String {
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

pub fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

pub fn parse_jobs(json: &str) -> Vec<Job> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    value["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .map(|job| {
                    let started = job["started_at"].as_str().and_then(parse_timestamp);
                    let completed = job["completed_at"].as_str().and_then(parse_timestamp);
                    Job {
                        name: job["name"].as_str().unwrap_or_default().to_owned(),
                        conclusion: job["conclusion"].as_str().map(str::to_owned),
                        seconds: match (started, completed) {
                            (Some(s), Some(c)) if c >= s => u64::try_from(c - s).ok(),
                            _ => None,
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_LOG: &str = "\u{1b}[1m\u{1b}[32m     Running\u{1b}[0m unittests src/lib.rs\n\
running 3 tests\n\
test a::one ... ok\n\
test a::two ... FAILED\n\
test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\
running 2 tests\n\
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
failures:\n\
test a::two ... FAILED\n";

    fn timeline(failed: bool, blocked: bool) -> String {
        let mut lines = vec![
            r#"{"suite":"smoke","task":"focus-window","at_ms":0,"took_ms":50,"kind":"call","tool":"begin_control_session","is_error":false}"#.to_owned(),
            r#"{"suite":"smoke","task":"focus-window","at_ms":50,"took_ms":120,"kind":"call","tool":"click_mouse","is_error":false}"#.to_owned(),
            r#"{"suite":"smoke","task":"focus-window","kind":"outcome","outcome":"passed","took_ms":940}"#.to_owned(),
            r#"{"suite":"handoff","task":"carry-value-read-by-ocr","at_ms":0,"took_ms":91,"kind":"call","tool":"read_text_in_region","is_error":false}"#.to_owned(),
            r#"{"suite":"handoff","task":"carry-value-read-by-ocr","at_ms":91,"took_ms":44,"kind":"expect","window":"ledger","what":"the invoice","met":true}"#.to_owned(),
        ];
        lines.push(if failed {
            r#"{"suite":"handoff","task":"carry-value-read-by-ocr","kind":"outcome","outcome":"failed","took_ms":2710,"failure":{"kind":"assertion","message":"OCR read \"INV 12B0\" @maintainer ~~~ | <b>"}}"#.to_owned()
        } else {
            r#"{"suite":"handoff","task":"carry-value-read-by-ocr","kind":"outcome","outcome":"passed","took_ms":2610}"#.to_owned()
        });
        if blocked {
            lines.push(r#"{"suite":"smoke","task":"click-text-by-ocr","kind":"outcome","outcome":"blocked","took_ms":80,"missing":["ocr"]}"#.to_owned());
        }
        lines.join("\n")
    }

    const RUN_JSON: &str = r#"{"tier":"pr","strict":true,"tools":["begin_control_session","click_mouse","drag_mouse","read_text_in_region"]}"#;

    fn jobs(tests: u64, e2e: u64) -> Vec<Job> {
        vec![
            Job {
                name: WORKSPACE_JOB.into(),
                conclusion: Some("success".into()),
                seconds: Some(tests),
            },
            Job {
                name: E2E_JOB.into(),
                conclusion: Some("success".into()),
                seconds: Some(e2e),
            },
        ]
    }

    fn results(failed: bool, blocked: bool) -> Results {
        Results {
            commit: "9f3c2e1abcdef".into(),
            workspace: Some(CargoTests {
                passed: 445,
                failed: 0,
                ignored: 12,
                failures: Vec::new(),
            }),
            e2e: Some(parse_e2e(&timeline(failed, blocked), Some(RUN_JSON))),
            jobs: jobs(123, 98),
        }
    }

    #[test]
    fn cargo_test_logs_are_summed_and_failures_named_once() {
        let tests = parse_cargo_test(CARGO_LOG).expect("results");
        assert_eq!((tests.passed, tests.failed, tests.ignored), (2, 1, 2));
        assert_eq!(tests.failures, vec!["a::two".to_owned()]);
        assert!(parse_cargo_test("error: could not compile").is_none());
    }

    #[test]
    fn a_green_run_has_a_verdict_lanes_and_folded_details() {
        let current = results(false, false);
        let baseline = Results {
            commit: "072592f00".into(),
            jobs: jobs(123, 91),
            ..results(false, false)
        };
        let body = render(&Report {
            current: &current,
            baseline: Some(&baseline),
            run_url: "https://example.invalid/run",
        });
        assert!(body.starts_with(MARKER));
        assert!(
            body.contains("### ✅ All tests passed · 447 tests · 2m 03s"),
            "{body}"
        );
        assert!(
            body.contains("| Live e2e (PR tier) | ✅ 2 passed | ±0 | 1m 38s | +7s |"),
            "{body}"
        );
        assert!(
            body.contains("Tool coverage: 3 of 4 tools called"),
            "{body}"
        );
        assert!(body.contains("Never called (1): `drag_mouse`"), "{body}");
        assert!(body.contains("```mermaid"));
        assert!(!body.contains("[!CAUTION]"));
        assert!(body.contains("baseline `main@072592f`"));
    }

    #[test]
    fn a_failure_is_shown_first_and_untrusted_text_is_neutralised() {
        let current = results(true, false);
        let body = render(&Report {
            current: &current,
            baseline: None,
            run_url: "u",
        });
        assert!(body.contains("### ❌ 1 failing · 446 passed"), "{body}");
        assert!(
            body.contains("> **handoff/`carry-value-read-by-ocr`** failed after 2.71s (assertion)"),
            "{body}"
        );
        assert!(
            body.contains("> ~~~text\n> OCR read \"INV 12B0\" @maintainer ~ ~ ~ | <b>\n> ~~~"),
            "{body}"
        );
        assert!(body.contains("cargo xtask e2e --suite handoff --strict"));
        assert!(body.contains("Last steps before the failure"));
        assert!(body.contains(":crit, "));
        assert!(body.contains("no baseline from `main` yet"));
        let caution = body.find("[!CAUTION]").unwrap();
        let lanes = body.find("| Lane |").unwrap();
        assert!(caution < lanes);
    }

    #[test]
    fn blocked_tasks_under_strict_get_a_warning_not_a_caution() {
        let current = results(false, true);
        let body = render(&Report {
            current: &current,
            baseline: None,
            run_url: "u",
        });
        assert!(body.contains("### ⛔ 1 e2e tasks blocked"), "{body}");
        assert!(
            body.contains("> - smoke/`click-text-by-ocr` (needs `ocr`)"),
            "{body}"
        );
        assert!(body.contains("⛔ 1 blocked · 2 passed"), "{body}");
    }

    #[test]
    fn a_job_that_died_before_results_is_reported() {
        let current = Results {
            commit: "abc".into(),
            workspace: None,
            e2e: None,
            jobs: vec![Job {
                name: WORKSPACE_JOB.into(),
                conclusion: Some("failure".into()),
                seconds: Some(40),
            }],
        };
        let body = render(&Report {
            current: &current,
            baseline: None,
            run_url: "u",
        });
        assert!(
            body.contains("### ❌ A test job failed before it produced results"),
            "{body}"
        );
        assert!(body.contains("| Workspace | ❌ job failure |"), "{body}");
        assert!(
            body.contains("| Live e2e (PR tier) | ⏳ running |"),
            "{body}"
        );
    }

    #[test]
    fn a_huge_report_is_trimmed_below_the_comment_limit() {
        let mut current = results(true, false);
        let e2e = current.e2e.as_mut().unwrap();
        for i in 0..3000 {
            let mut task = e2e.tasks[0].clone();
            task.task = format!("task-{i}");
            e2e.tasks.push(task);
            e2e.calls.push((format!("tool_{i}"), 5));
        }
        let body = render(&Report {
            current: &current,
            baseline: None,
            run_url: "u",
        });
        assert!(body.len() <= LIMIT + 200, "{}", body.len());
        assert!(body.starts_with(MARKER));
        assert!(body.contains("[!CAUTION]"));
        assert!(!body.contains("```mermaid"));
    }

    #[test]
    fn plain_text_cannot_mention_or_inject_markup() {
        assert_eq!(
            plain("@team <img> | *x*"),
            "@\u{200b}team \\<img\\> \\| \\*x\\*"
        );
        assert_eq!(code("a`b|c"), "`ab\\|c`");
        assert_eq!(ident("smoke; rm -rf"), "smokerm-rf");
    }

    #[test]
    fn job_durations_come_from_api_timestamps() {
        let jobs = parse_jobs(
            r#"{"jobs":[{"name":"Tests","conclusion":"success","started_at":"2026-11-14T13:36:43Z","completed_at":"2026-11-14T13:38:46Z"},{"name":"Live desktop tests","conclusion":null,"started_at":"2026-11-14T23:59:59Z","completed_at":null}]}"#,
        );
        assert_eq!(jobs[0].seconds, Some(123));
        assert_eq!(jobs[1].seconds, None);
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2000-03-01T00:00:00Z"), Some(951_868_800));
    }

    #[test]
    fn the_running_comment_carries_the_marker() {
        let body = render_running("9f3c2e1abc", "https://example.invalid/r");
        assert!(body.starts_with(MARKER) && body.contains("`9f3c2e1`"));
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let values: Vec<u64> = (1..=20).collect();
        assert_eq!(percentile(&values, 50), 10);
        assert_eq!(percentile(&values, 95), 19);
        assert_eq!(percentile(&[7], 95), 7);
    }
}
