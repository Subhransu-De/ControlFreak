use crate::{events, mcp::McpClient, scene::WindowSpec};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const GATE: &str = "CONTROLFREAK_LIVE_DESKTOP";
const SERVER_EXE: &str = "controlfreak.exe";
const FIXTURE_EXE: &str = "controlfreak-fixture.exe";
const READY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn require_live_desktop() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("live suites drive a Windows desktop and need Windows".into());
    }
    if std::env::var(GATE).as_deref() != Ok("1") {
        return Err(format!(
            "refusing to run: the suites inject real input and change the foreground window. \
             Run them only on a disposable desktop (a CI runner or a test VM) with {GATE}=1."
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Binaries {
    server: PathBuf,
    fixture: PathBuf,
}

impl Binaries {
    pub fn locate(dir: Option<&Path>) -> Result<Self, String> {
        let dir = match dir {
            Some(dir) => dir.to_path_buf(),
            None => std::env::current_exe()
                .map_err(|error| format!("cannot find the harness binary: {error}"))?
                .parent()
                .map(Path::to_path_buf)
                .ok_or("the harness binary has no parent directory")?,
        };
        let binaries = Self {
            server: dir.join(SERVER_EXE),
            fixture: dir.join(FIXTURE_EXE),
        };
        for path in [&binaries.server, &binaries.fixture] {
            if !path.is_file() {
                return Err(format!(
                    "{} not found: build it with `cargo build -p controlfreak-server \
                     -p controlfreak-live-tests --locked`, or run `cargo xtask e2e`",
                    path.display()
                ));
            }
        }
        Ok(binaries)
    }

    pub(crate) fn launch_server(&self, stderr_log: &Path) -> Result<McpClient, String> {
        let stderr = fs::File::create(stderr_log)
            .map_err(|error| format!("cannot create {}: {error}", stderr_log.display()))?;
        let mut child = Command::new(&self.server)
            .arg("--allow-elevated")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", self.server.display()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the server has no STDIO pipes".into());
        };
        McpClient::connect(stdout, stdin, Some(child), crate::mcp::DEFAULT_TIMEOUT)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn open_window(
        &self,
        spec: &WindowSpec,
        unique: &str,
        log_dir: &Path,
    ) -> Result<Window, String> {
        let title = format!("CFE2E {} {unique}", spec.key);
        let log = log_dir.join(format!("{}-{unique}.log", spec.key));
        let mut command = Command::new(&self.fixture);
        command
            .args(["--title", &title, "--color", &spec.color])
            .args(["--lifetime-secs", &spec.lifetime_secs.to_string()])
            .args([
                "--pos",
                &format!("{},{}", spec.position[0], spec.position[1]),
            ])
            .arg("--log")
            .arg(&log)
            .stdout(Stdio::null());
        if spec.topmost {
            command.arg("--topmost");
        }
        if spec.dialog {
            command.arg("--with-dialog");
        }
        if spec.busy_drain_ms > 0 {
            command.args(["--busy-drain-ms", &spec.busy_drain_ms.to_string()]);
        }
        if spec.clip_cursor {
            command.arg("--clip-cursor");
        }
        if spec.popup_on_ping {
            command.arg("--popup-on-ping");
        }
        let child = command
            .spawn()
            .map_err(|error| format!("cannot open fixture {}: {error}", spec.key))?;
        let mut window = Window {
            key: spec.key.clone(),
            title,
            log,
            child,
            regions: events::Regions::default(),
            release_clip_with: spec.clip_cursor.then(|| self.fixture.clone()),
        };
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(regions) = events::ready(&window.events()) {
                window.regions = regions.clone();
                return Ok(window);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "fixture {} never logged its ready line within {READY_TIMEOUT:?}",
                    spec.key
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn fixture_helper(&self, args: &[&str]) -> Result<Output, String> {
        Command::new(&self.fixture)
            .args(args)
            .output()
            .map_err(|error| format!("fixture helper {args:?} did not start: {error}"))
    }
}

pub struct Window {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) log: PathBuf,
    child: Child,
    pub(crate) regions: events::Regions,
    /// The fixture executable, for a window that confines the pointer: killing it may leave
    /// the pointer clipped, so the pointer is freed after the kill, whatever the task did.
    release_clip_with: Option<PathBuf>,
}

impl Window {
    pub(crate) fn process_id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn events(&self) -> Vec<events::Event> {
        events::parse_log(&fs::read_to_string(&self.log).unwrap_or_default())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(fixture) = &self.release_clip_with {
            let released = Command::new(fixture)
                .arg("--release-clip")
                .status()
                .is_ok_and(|status| status.success());
            if !released {
                eprintln!("could not free the pointer clip of fixture {}", self.key);
            }
        }
    }
}
