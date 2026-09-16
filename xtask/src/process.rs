//! Bounded child execution without a shell or global environment mutation.
use crate::Result;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Inno Setup can start a child executable. Terminate only the tree
            // rooted at this captured PID while its owned process handle is live.
            #[cfg(windows)]
            stop_tree(self.0.id());
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(windows)]
fn stop_tree(pid: u32) {
    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let executable = std::path::PathBuf::from(system_root).join("System32/taskkill.exe");
    let Ok(mut child) = Command::new(executable)
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn run(command: &mut Command, input: &str, timeout: Duration) -> Result<Output> {
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::piped())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = OwnedChild(command.spawn()?);
    if let Some(mut stdin) = child.0.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            return Err("Child process exceeded its deadline".into());
        }
        thread::sleep(Duration::from_millis(50));
    };
    Ok(Output {
        status,
        stdout: text(&mut stdout)?,
        stderr: text(&mut stderr)?,
    })
}

fn text(file: &mut File) -> Result<String> {
    if file.metadata()?.len() > 4 * 1024 * 1024 {
        return Err("Child output exceeded safety limit".into());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

pub fn checked(command: &mut Command, timeout: Duration) -> Result<Output> {
    let output = run(command, "", timeout)?;
    // Captured output may contain environment/configuration details. Do not echo it.
    if !output.status.success() {
        return Err("Child command failed; captured output was not published".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_program_is_reported() {
        assert!(
            checked(
                Command::new("controlfreak-synthetic-missing.exe").arg("fixture"),
                Duration::from_secs(1)
            )
            .is_err()
        );
    }

    #[test]
    fn deadline_stops_an_owned_child() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "process::tests::synthetic_child", "--nocapture"])
            .env("CONTROLFREAK_SYNTHETIC_XTASK_CHILD", "sleep");
        assert!(run(&mut command, "", Duration::from_millis(100)).is_err());
    }

    #[test]
    fn synthetic_child() {
        if std::env::var("CONTROLFREAK_SYNTHETIC_XTASK_CHILD").as_deref() == Ok("sleep") {
            thread::sleep(Duration::from_secs(10));
        }
    }
}
