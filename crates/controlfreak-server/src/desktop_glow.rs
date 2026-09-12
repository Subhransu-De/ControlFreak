use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use controlfreak_core::MutationControl;
use controlfreak_mcp::{ActivityIndicator, IndicatorHealth, IndicatorLevel};
use controlfreak_platform::ManagedChild;

const TEST_DISABLE_ENV: &str = "CONTROLFREAK_DISABLE_DESKTOP_GLOW_FOR_TESTS";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const ACTOR_POLL: Duration = Duration::from_millis(50);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Owns one hidden, reusable desktop-glow actor for an MCP server.
pub(crate) struct DesktopGlow {
    actor: Option<GlowActor>,
    health: Arc<Mutex<IndicatorHealth>>,
    cancellation: MutationControl,
    containment: (&'static str, Option<String>),
    draining: Option<JoinHandle<()>>,
    // Terminal actor panic only; helper termination errors are retried by the actor.
    shutdown_failure: Option<String>,
}

struct GlowActor {
    sender: mpsc::SyncSender<ActorRequest>,
    thread: Option<JoinHandle<()>>,
}

enum ActorRequest {
    Command {
        command: GlowCommand,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Shutdown {
        reply: mpsc::SyncSender<Result<(), String>>,
    },
}

enum GlowCommand {
    Level(IndicatorLevel),
    Hide,
}

enum ProcessEvent {
    Line(io::Result<String>),
    StdoutClosed,
}

struct PendingCommand {
    expected: &'static str,
    deadline: Instant,
    reply: Option<mpsc::SyncSender<Result<(), String>>>,
    shutdown: bool,
}

struct ActorProcess {
    child: ManagedChild,
    stdin: File,
    events: mpsc::Receiver<ProcessEvent>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
}

impl DesktopGlow {
    pub(crate) fn start(elevated: bool) -> Result<Self, String> {
        let health = Arc::new(Mutex::new(IndicatorHealth::Healthy));
        let cancellation = MutationControl::default();
        if cfg!(debug_assertions) && std::env::var_os(TEST_DISABLE_ENV).is_some() {
            return Ok(Self {
                actor: None,
                draining: None,
                shutdown_failure: None,
                health,
                cancellation,
                containment: (
                    "unavailable",
                    Some("desktop glow is disabled for tests".to_owned()),
                ),
            });
        }

        let generation = next_generation();
        let process = spawn_glow_process(generation).map_err(|error| {
            record_glow_error(&error);
            error.to_string()
        })?;
        let process = await_startup(process, generation).map_err(|error| {
            record_glow_error(&error);
            error.to_string()
        })?;
        let containment = process.child.containment();
        let (sender, receiver) = mpsc::sync_channel(16);
        let health_for_actor = Arc::clone(&health);
        let cancellation_for_actor = cancellation.clone();
        let actor = thread::Builder::new()
            .name("controlfreak-glow-actor".to_owned())
            .spawn(move || {
                actor_loop(
                    process,
                    generation,
                    &receiver,
                    &health_for_actor,
                    &cancellation_for_actor,
                    elevated,
                );
            })
            .map_err(|error| format!("desktop glow actor could not start: {error}"))?;
        Ok(Self {
            draining: None,
            shutdown_failure: None,
            actor: Some(GlowActor {
                sender,
                thread: Some(actor),
            }),
            health,
            cancellation,
            containment,
        })
    }

    fn finish_shutdown(&mut self, result: Result<(), String>) -> Result<(), String> {
        let result = finish_actor_shutdown(&mut self.draining, result);
        if self.draining.is_none() {
            self.shutdown_failure = result.as_ref().err().cloned();
        }
        result
    }

    fn command(&self, command: GlowCommand) -> Result<(), String> {
        if self.draining.is_some() || self.shutdown_failure.is_some() {
            return Err("desktop glow actor is still draining".to_owned());
        }
        let Some(actor) = self.actor.as_ref() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        actor
            .sender
            .send(ActorRequest::Command {
                command,
                reply: reply_tx,
            })
            .map_err(|error| format!("desktop glow actor stopped: {error}"))?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT + Duration::from_secs(1))
            .map_err(|error| format!("desktop glow command reply timed out: {error}"))?
    }
}

impl ActivityIndicator for DesktopGlow {
    fn set_level(&mut self, level: IndicatorLevel) -> Result<(), String> {
        self.command(GlowCommand::Level(level))
    }

    fn hide(&mut self) -> Result<(), String> {
        self.command(GlowCommand::Hide)
    }

    fn shutdown(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.shutdown_failure {
            return Err(reason.clone());
        }
        if self.draining.is_some() {
            return self.finish_shutdown(Ok(()));
        }
        let Some(mut actor) = self.actor.take() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let send = actor
            .sender
            .send(ActorRequest::Shutdown { reply: reply_tx })
            .map_err(|error| format!("desktop glow actor stopped before shutdown: {error}"));
        let result = send.and_then(|()| {
            reply_rx
                .recv_timeout(COMMAND_TIMEOUT + PROCESS_EXIT_TIMEOUT)
                .map_err(|error| format!("desktop glow shutdown timed out: {error}"))?
        });
        self.draining = actor.thread.take();
        self.finish_shutdown(result)
    }

    fn health(&self) -> IndicatorHealth {
        self.health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn mutation_control(&self) -> MutationControl {
        self.cancellation.clone()
    }

    fn containment(&self) -> (&'static str, Option<String>) {
        self.containment.clone()
    }
}

impl Drop for DesktopGlow {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn finish_actor_shutdown(
    thread: &mut Option<JoinHandle<()>>,
    result: Result<(), String>,
) -> Result<(), String> {
    let Some(handle) = thread.as_ref() else {
        return result;
    };
    if result.is_err() && !handle.is_finished() {
        return result;
    }
    let deadline = Instant::now() + PROCESS_EXIT_TIMEOUT;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if handle.is_finished() {
        thread
            .take()
            .expect("actor handle is present")
            .join()
            .map_err(|_| "desktop glow actor panicked before confirming helper exit".to_owned())
    } else {
        Err("desktop glow actor is still draining after bounded shutdown".to_owned())
    }
}

#[allow(clippy::too_many_lines)]
fn actor_loop(
    mut process: ActorProcess,
    generation: u64,
    requests: &mpsc::Receiver<ActorRequest>,
    health: &Mutex<IndicatorHealth>,
    cancellation: &MutationControl,
    elevated: bool,
) {
    let mut sequence = 1_u64;
    let mut pending = HashMap::<u64, PendingCommand>::new();
    let mut visible = false;
    let mut next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
    let mut shutdown_requested = false;
    let mut shutdown_acknowledged = false;

    loop {
        while let Ok(request) = requests.try_recv() {
            match request {
                ActorRequest::Command { command, reply } => {
                    if !matches!(current_health(health), IndicatorHealth::Healthy) {
                        let _ = reply.send(Err("desktop glow helper is unhealthy".to_owned()));
                        continue;
                    }
                    let (wire_command, expected) = match (command, elevated) {
                        (GlowCommand::Level(IndicatorLevel::Armed), false) => {
                            ("LEVEL ARMED", "ARMED")
                        }
                        (GlowCommand::Level(IndicatorLevel::Acting), false) => {
                            ("LEVEL ACTING", "ACTING")
                        }
                        (GlowCommand::Level(IndicatorLevel::Armed), true) => {
                            ("LEVEL ELEVATED_ARMED", "ELEVATED_ARMED")
                        }
                        (GlowCommand::Level(IndicatorLevel::Acting), true) => {
                            ("LEVEL ELEVATED_ACTING", "ELEVATED_ACTING")
                        }
                        (GlowCommand::Hide, _) => ("HIDE", "HIDDEN"),
                    };
                    if let Err(error) =
                        send_command(&mut process.stdin, generation, sequence, wire_command)
                    {
                        let reason = format!("desktop glow command write failed: {error}");
                        mark_unhealthy(health, cancellation, reason.clone());
                        let _ = reply.send(Err(reason));
                    } else {
                        pending.insert(
                            sequence,
                            PendingCommand {
                                expected,
                                deadline: Instant::now() + COMMAND_TIMEOUT,
                                reply: Some(reply),
                                shutdown: false,
                            },
                        );
                        sequence = sequence.wrapping_add(1);
                    }
                }
                ActorRequest::Shutdown { reply } => {
                    shutdown_requested = true;
                    if matches!(current_health(health), IndicatorHealth::Healthy) {
                        if let Err(error) =
                            send_command(&mut process.stdin, generation, sequence, "SHUTDOWN")
                        {
                            let _ = reply
                                .send(Err(format!("desktop glow shutdown write failed: {error}")));
                        } else {
                            pending.insert(
                                sequence,
                                PendingCommand {
                                    expected: "STOPPED",
                                    deadline: Instant::now() + COMMAND_TIMEOUT,
                                    reply: Some(reply),
                                    shutdown: true,
                                },
                            );
                            sequence = sequence.wrapping_add(1);
                        }
                    } else {
                        let result = stop_process_bounded(&mut process);
                        let failed = result.is_err();
                        let _ = reply.send(result);
                        if failed {
                            drain_until_stopped(|| stop_process_bounded(&mut process));
                        }
                        return;
                    }
                }
            }
        }

        match process.events.recv_timeout(ACTOR_POLL) {
            Ok(ProcessEvent::Line(Ok(line))) => match parse_marker(&line) {
                Ok(marker) if marker.generation != generation => {}
                Ok(marker) => {
                    if let Some(command) = pending.remove(&marker.sequence) {
                        if marker.state == command.expected {
                            visible = matches!(
                                marker.state.as_str(),
                                "ARMED" | "ACTING" | "ELEVATED_ARMED" | "ELEVATED_ACTING"
                            );
                            next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
                            shutdown_acknowledged |= command.shutdown;
                            if let Some(reply) = command.reply {
                                let _ = reply.send(Ok(()));
                            }
                        } else {
                            let reason = format!(
                                "desktop glow returned '{}' while waiting for '{}'",
                                marker.state, command.expected
                            );
                            mark_unhealthy(health, cancellation, reason.clone());
                            if let Some(reply) = command.reply {
                                let _ = reply.send(Err(reason));
                            }
                        }
                    }
                }
                Err(reason) => mark_unhealthy(health, cancellation, reason),
            },
            Ok(ProcessEvent::Line(Err(error))) => mark_unhealthy(
                health,
                cancellation,
                format!("desktop glow stdout read failed: {error}"),
            ),
            Ok(ProcessEvent::StdoutClosed) => mark_unhealthy(
                health,
                cancellation,
                "desktop glow stdout closed before child exit".to_owned(),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => mark_unhealthy(
                health,
                cancellation,
                "desktop glow output observers stopped".to_owned(),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if let Ok(Some(status)) = process.child.try_wait() {
            mark_dead(
                health,
                cancellation,
                format!("desktop glow helper exited with {status}"),
            );
            fail_pending(&mut pending, "desktop glow helper exited");
            join_readers(&mut process);
            return;
        }

        let now = Instant::now();
        let expired: Vec<u64> = pending
            .iter()
            .filter_map(|(sequence, command)| (now >= command.deadline).then_some(*sequence))
            .collect();
        for sequence in expired {
            if let Some(command) = pending.remove(&sequence) {
                let reason = format!(
                    "desktop glow did not acknowledge '{}' before its deadline; {}",
                    command.expected,
                    failure_context(&mut process)
                );
                mark_unhealthy(health, cancellation, reason.clone());
                if let Some(reply) = command.reply {
                    let _ = reply.send(Err(reason));
                }
            }
        }

        if visible
            && pending.is_empty()
            && now >= next_heartbeat
            && matches!(current_health(health), IndicatorHealth::Healthy)
            && send_command(&mut process.stdin, generation, sequence, "PING").is_ok()
        {
            pending.insert(
                sequence,
                PendingCommand {
                    expected: "PONG",
                    deadline: now + HEARTBEAT_TIMEOUT,
                    reply: None,
                    shutdown: false,
                },
            );
            sequence = sequence.wrapping_add(1);
        }

        if shutdown_requested
            && (shutdown_acknowledged
                || !matches!(current_health(health), IndicatorHealth::Healthy))
        {
            drain_until_stopped(|| stop_process_bounded(&mut process));
            return;
        }
    }
}

fn await_startup(mut process: ActorProcess, generation: u64) -> io::Result<ActorProcess> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match process.events.recv_timeout(remaining) {
            Ok(ProcessEvent::Line(Ok(line))) => {
                let marker = parse_marker(&line)
                    .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?;
                if marker.generation == generation
                    && marker.sequence == 0
                    && marker.state == "HIDDEN"
                {
                    return Ok(process);
                }
            }
            Ok(ProcessEvent::Line(Err(error))) => {
                let context = stop_and_describe_startup_failure(&mut process);
                return Err(io::Error::new(
                    error.kind(),
                    format!("desktop glow stdout read failed: {error}; {context}"),
                ));
            }
            Ok(ProcessEvent::StdoutClosed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let context = stop_and_describe_startup_failure(&mut process);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("desktop glow output closed during startup; {context}"),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let context = stop_and_describe_startup_failure(&mut process);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("desktop glow did not report correlated HIDDEN startup: {context}"),
                ));
            }
        }
    }
}

fn stop_and_describe_startup_failure(process: &mut ActorProcess) -> String {
    let stop_error = stop_process_bounded(process).err();
    let context = failure_context(process);
    stop_error.map_or(context.clone(), |error| {
        format!("{context}; cleanup error: {error}")
    })
}

struct Marker {
    generation: u64,
    sequence: u64,
    state: String,
}

fn parse_marker(line: &str) -> Result<Marker, String> {
    let fields: Vec<&str> = line.split_ascii_whitespace().collect();
    if fields.len() != 4 || fields[0] != "CFP/1" {
        return Err(format!(
            "desktop glow returned malformed protocol line {line:?}"
        ));
    }
    let generation = fields[1]
        .parse::<u64>()
        .map_err(|error| format!("desktop glow generation was invalid: {error}"))?;
    let sequence = fields[2]
        .parse::<u64>()
        .map_err(|error| format!("desktop glow sequence was invalid: {error}"))?;
    Ok(Marker {
        generation,
        sequence,
        state: fields[3].to_owned(),
    })
}

fn send_command(stdin: &mut File, generation: u64, sequence: u64, command: &str) -> io::Result<()> {
    writeln!(stdin, "CFP/1 {generation} {sequence} {command}")?;
    stdin.flush()
}

fn mark_unhealthy(health: &Mutex<IndicatorHealth>, cancellation: &MutationControl, reason: String) {
    let mut current = health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(*current, IndicatorHealth::Healthy) {
        *current = IndicatorHealth::Unhealthy(reason);
        cancellation.cancel();
    }
}

fn mark_dead(health: &Mutex<IndicatorHealth>, cancellation: &MutationControl, reason: String) {
    *health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = IndicatorHealth::Dead(reason);
    cancellation.cancel();
}

fn current_health(health: &Mutex<IndicatorHealth>) -> IndicatorHealth {
    health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn fail_pending(pending: &mut HashMap<u64, PendingCommand>, reason: &str) {
    for (_, command) in pending.drain() {
        if let Some(reply) = command.reply {
            let _ = reply.send(Err(reason.to_owned()));
        }
    }
}

// Keep the process and pipe handles in the actor until exit is confirmed.
// Callers wait only for the bounded shutdown attempt and can retry later.
fn drain_until_stopped(mut stop: impl FnMut() -> Result<(), String>) {
    while stop().is_err() {
        thread::sleep(ACTOR_POLL);
    }
}

fn stop_process_bounded(process: &mut ActorProcess) -> Result<(), String> {
    if process.child.try_wait().ok().flatten().is_none() {
        process
            .child
            .terminate()
            .map_err(|error| format!("desktop glow helper could not be terminated: {error}"))?;
    }
    let deadline = Instant::now() + PROCESS_EXIT_TIMEOUT;
    loop {
        match process.child.try_wait() {
            Ok(Some(_)) => {
                join_readers(process);
                return Ok(());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                return Err("desktop glow helper did not exit after bounded termination".to_owned());
            }
            Err(error) => {
                return Err(format!(
                    "desktop glow exit status could not be confirmed: {error}"
                ));
            }
        }
    }
}

fn join_readers(process: &mut ActorProcess) {
    if let Some(reader) = process.stdout_reader.take() {
        let _ = reader.join();
    }
    if let Some(reader) = process.stderr_reader.take() {
        let _ = reader.join();
    }
}

fn failure_context(process: &mut ActorProcess) -> String {
    let status = process.child.try_wait().ok().flatten().map_or_else(
        || "exit status unavailable".to_owned(),
        |status| status.to_string(),
    );
    let stderr = process
        .stderr_lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .join(" | ");
    if stderr.is_empty() {
        format!("native helper {status}, stderr was empty")
    } else {
        format!("native helper {status}, stderr: {stderr}")
    }
}

fn spawn_glow_process(generation: u64) -> io::Result<ActorProcess> {
    let arguments = vec![
        OsString::from("--desktop-glow-helper"),
        OsString::from("--protocol-generation"),
        OsString::from(generation.to_string()),
    ];
    let executable = std::env::current_exe()?;
    let mut child = controlfreak_platform::spawn_helper_process(&executable, &arguments)?;
    record_glow_event(&format!(
        "native desktop glow spawned as {} generation {generation}",
        child.id()
    ));
    let stdin = take_pipe(child.take_stdin(), "stdin")?;
    let stdout = take_pipe(child.take_stdout(), "stdout")?;
    let stderr = take_pipe(child.take_stderr(), "stderr")?;
    let (event_tx, event_rx) = mpsc::channel();
    let stdout_reader = spawn_stdout_reader(stdout, event_tx)?;
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_reader = spawn_stderr_reader(stderr, Arc::clone(&stderr_lines))?;
    Ok(ActorProcess {
        child,
        stdin,
        events: event_rx,
        stderr_lines,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
    })
}

fn take_pipe<T>(pipe: Option<T>, name: &str) -> io::Result<T> {
    pipe.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("desktop glow {name} was unavailable"),
        )
    })
}

fn spawn_stdout_reader(
    stdout: File,
    sender: mpsc::Sender<ProcessEvent>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("controlfreak-glow-output".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(ProcessEvent::Line(line)).is_err() {
                    return;
                }
            }
            let _ = sender.send(ProcessEvent::StdoutClosed);
        })
}

fn spawn_stderr_reader(stderr: File, lines: Arc<Mutex<Vec<String>>>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("controlfreak-glow-errors".to_owned())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap_or_else(|error| format!("stderr read error: {error}"));
                record_glow_event(&format!("desktop glow stderr: {line}"));
                lines
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(line);
            }
        })
}

fn next_generation() -> u64 {
    let counter = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        });
    time ^ (u64::from(std::process::id()) << 32) ^ counter
}

fn record_glow_error(error: &dyn std::fmt::Display) {
    record_glow_event(&format!("error={error}"));
}

fn record_glow_event(message: &str) {
    let Some(path) = std::env::var_os("CONTROLFREAK_GLOW_ERROR_LOG") else {
        return;
    };
    if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(log, "process_id={} {message}", std::process::id());
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopGlow, finish_actor_shutdown, parse_marker};
    use controlfreak_mcp::{ActivityIndicator, IndicatorHealth};

    #[test]
    fn failed_actor_shutdown_does_not_join_a_wedged_thread() {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let actor = std::thread::spawn(move || {
            let _ = release_rx.recv();
        });
        let started = std::time::Instant::now();

        let mut actor = Some(actor);
        let error =
            finish_actor_shutdown(&mut actor, Err("shutdown timed out".to_owned())).unwrap_err();

        assert_eq!(error, "shutdown timed out");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(actor.is_some());
        let _ = release_tx.send(());
        finish_actor_shutdown(&mut actor, Ok(())).unwrap();
        assert!(actor.is_none());
    }

    #[test]
    fn failed_helper_termination_is_retried_before_actor_exit() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let actor_stopped = Arc::clone(&stopped);
        let (attempted, receiver) = mpsc::channel();
        let actor = std::thread::spawn(move || {
            super::drain_until_stopped(|| {
                if actor_stopped.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    let _ = attempted.send(());
                    Err("helper termination failed".to_owned())
                }
            });
        });
        let mut glow = DesktopGlow {
            actor: None,
            health: Arc::new(std::sync::Mutex::new(IndicatorHealth::Healthy)),
            cancellation: controlfreak_core::MutationControl::default(),
            containment: ("unavailable", None),
            draining: Some(actor),
            shutdown_failure: None,
        };
        receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let failed = glow.finish_shutdown(Err("helper termination failed".to_owned()));
        let still_draining = glow.draining.is_some();
        let hidden = glow.hide();
        stopped.store(true, Ordering::Release);
        assert!(failed.is_err());
        assert!(still_draining);
        assert!(hidden.is_err());
        glow.shutdown().unwrap();
        glow.shutdown().unwrap();
        assert!(glow.draining.is_none());
    }

    #[test]
    fn correlated_marker_parser_rejects_uncorrelated_output() {
        let marker = parse_marker("CFP/1 42 7 ACTING").unwrap();
        assert_eq!(marker.generation, 42);
        assert_eq!(marker.sequence, 7);
        assert_eq!(marker.state, "ACTING");
        assert_eq!(
            parse_marker("CFP/1 42 8 ELEVATED_ACTING").unwrap().state,
            "ELEVATED_ACTING"
        );
        assert!(parse_marker("CONTROLFREAK_GLOW_VISIBLE").is_err());
    }

    #[test]
    #[ignore = "starts the real hidden Windows helper for release qualification"]
    fn live_hidden_helper_starts_healthy_and_stops() {
        let mut glow = DesktopGlow::start(false).expect("desktop glow helper should start");
        assert_eq!(glow.health(), IndicatorHealth::Healthy);
        glow.shutdown().expect("desktop glow helper should stop");
    }
}
