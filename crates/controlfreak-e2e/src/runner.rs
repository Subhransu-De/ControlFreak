use crate::{
    host::{Binaries, Window},
    mcp::McpClient,
    scene::{Scene, WindowSpec},
    workflow::{Capability, Ctx, Failure, FailureKind, Step, Suite, Task, Tier},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Output,
    time::{Duration, Instant},
};

pub type LaunchServer = Box<dyn FnMut(&Path) -> Result<McpClient, String>>;
pub type OpenWindow = Box<dyn FnMut(&WindowSpec, &str, &Path) -> Result<Window, String>>;
pub type Helper = Box<dyn FnMut(&[&str]) -> Result<Output, String>>;

pub struct Environment {
    pub launch_server: LaunchServer,
    pub open_window: OpenWindow,
    pub helper: Helper,
}

impl Environment {
    pub fn windows(binaries: Binaries) -> Self {
        let (server, windows, helper) = (binaries.clone(), binaries.clone(), binaries);
        Self {
            launch_server: Box::new(move |log| server.launch_server(log)),
            open_window: Box::new(move |spec, unique, dir| windows.open_window(spec, unique, dir)),
            helper: Box::new(move |args| helper.fixture_helper(args)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub tier: Tier,
    pub strict: bool,
    pub suites: Vec<String>,
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed(Failure),
    Blocked(Vec<Capability>),
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub suite: &'static str,
    pub task: &'static str,
    pub outcome: Outcome,
    pub elapsed: Duration,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub tier: Tier,
    pub strict: bool,
    pub records: Vec<TaskRecord>,
    pub elapsed: Duration,
    pub tools: Vec<String>,
}

impl RunReport {
    pub fn count(&self, matches: impl Fn(&Outcome) -> bool) -> usize {
        self.records.iter().filter(|r| matches(&r.outcome)).count()
    }

    pub fn succeeded(&self) -> bool {
        self.records.iter().all(|record| match record.outcome {
            Outcome::Passed => true,
            Outcome::Failed(_) => false,
            Outcome::Blocked(_) => !self.strict,
        })
    }
}

pub fn select<'s>(
    suites: &'s [Suite],
    plan: &Plan,
) -> Result<Vec<(&'s Suite, Vec<&'s Task>)>, String> {
    for wanted in &plan.suites {
        if !suites.iter().any(|suite| suite.name == wanted) {
            let known: Vec<&str> = suites.iter().map(|suite| suite.name).collect();
            return Err(format!(
                "no suite named {wanted:?}; known: {}",
                known.join(", ")
            ));
        }
    }
    Ok(suites
        .iter()
        .filter(|suite| plan.suites.is_empty() || plan.suites.iter().any(|s| s == suite.name))
        .map(|suite| {
            let tasks = suite
                .tasks
                .iter()
                .filter(|task| plan.tier.includes(task.tier))
                .collect();
            (suite, tasks)
        })
        .filter(|(_, tasks): &(_, Vec<_>)| !tasks.is_empty())
        .collect())
}

pub fn run(
    suites: &[Suite],
    plan: &Plan,
    environment: &mut Environment,
    on_task: &mut dyn FnMut(&TaskRecord),
) -> Result<RunReport, String> {
    let selected = select(suites, plan)?;
    let logs = plan.out_dir.join("logs");
    std::fs::create_dir_all(&logs)
        .map_err(|error| format!("cannot create {}: {error}", logs.display()))?;
    let started = Instant::now();
    let mut records = Vec::new();
    let mut tools = Vec::new();
    for (suite, tasks) in selected {
        for task in tasks {
            let record = run_task(suite.name, task, environment, &logs, &mut tools);
            on_task(&record);
            records.push(record);
        }
    }
    Ok(RunReport {
        tier: plan.tier,
        strict: plan.strict,
        records,
        elapsed: started.elapsed(),
        tools,
    })
}

fn run_task(
    suite: &'static str,
    task: &Task,
    environment: &mut Environment,
    logs: &Path,
    tools: &mut Vec<String>,
) -> TaskRecord {
    let started = Instant::now();
    let unique = format!("{suite}-{}-{}", task.name, std::process::id());
    let (outcome, steps) = match prepare(task, environment, logs, &unique) {
        Err(failure) => (Outcome::Failed(failure), Vec::new()),
        Ok((mut client, windows)) => {
            if tools.is_empty() {
                tools.extend(client.tool_names().map(str::to_owned));
                tools.sort();
            }
            let mut ctx = Ctx::new(&mut client, &windows, &mut *environment.helper);
            let outcome = execute(task, &mut ctx, started);
            let steps = std::mem::take(&mut ctx.steps);
            drop(ctx);
            drop(client);
            drop(windows);
            (outcome, steps)
        }
    };
    TaskRecord {
        suite,
        task: task.name,
        outcome,
        elapsed: started.elapsed(),
        steps,
    }
}

fn prepare(
    task: &Task,
    environment: &mut Environment,
    logs: &Path,
    unique: &str,
) -> Result<(McpClient, Vec<Window>), Failure> {
    let scene = task
        .scene
        .map(Scene::builtin)
        .transpose()
        .map_err(Failure::host)?;
    let client = (environment.launch_server)(&logs.join(format!("{unique}.server.log")))
        .map_err(|error| Failure::new(FailureKind::Protocol, error))?;
    let mut windows = Vec::new();
    for spec in scene.iter().flat_map(|scene| &scene.windows) {
        windows.push((environment.open_window)(spec, unique, logs).map_err(Failure::host)?);
    }
    Ok((client, windows))
}

fn execute(task: &Task, ctx: &mut Ctx<'_>, started: Instant) -> Outcome {
    let mut missing = Vec::new();
    for &capability in task.requires {
        match ctx.probe(capability) {
            Ok(true) => {}
            Ok(false) => missing.push(capability),
            Err(failure) => return Outcome::Failed(failure),
        }
    }
    if !missing.is_empty() {
        return Outcome::Blocked(missing);
    }
    let body = task.body;
    let result = catch_unwind(AssertUnwindSafe(|| body(ctx))).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the task panicked".into());
        Err(Failure::assertion(format!("panic: {message}")))
    });
    match result {
        Err(failure) => Outcome::Failed(failure),
        Ok(()) if started.elapsed() > task.budget => Outcome::Failed(Failure::new(
            FailureKind::Timeout,
            format!(
                "passed but took {:.1}s, over its {}s budget",
                started.elapsed().as_secs_f64(),
                task.budget.as_secs()
            ),
        )),
        Ok(()) => Outcome::Passed,
    }
}
