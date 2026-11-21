//! Virtual desktops as Explorer records them, read through the fixture helper independently of
//! the server and of any window, and desktops the harness adds and removes with the shell's own
//! shortcuts. A task that adds a desktop removes it again and checks the runner is back on the
//! desktop it started on.

use crate::{Ctx, Failure};
use std::{
    thread,
    time::{Duration, Instant},
};

/// How long the shell may take to record a desktop change after its shortcut.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// The shell's current desktop and its desktops in order, as GUIDs in the server's format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Desktops {
    pub(crate) current: Option<String>,
    pub(crate) ids: Vec<String>,
}

impl Desktops {
    /// Parses the `--virtual-desktops` output: `current GUID|none`, then `ids GUID,…`.
    pub(crate) fn parse(output: &str) -> Result<Self, String> {
        let mut current = None;
        let mut ids = None;
        for line in output.lines() {
            match line.trim().split_once(' ').unwrap_or((line.trim(), "")) {
                ("current", "none") => current = Some(None),
                ("current", id) if !id.is_empty() => current = Some(Some(id.to_owned())),
                ("ids", list) => {
                    ids = Some(
                        list.split(',')
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    );
                }
                _ => return Err(format!("unexpected line {line:?}")),
            }
        }
        match (current, ids) {
            (Some(current), Some(ids)) => Ok(Self { current, ids }),
            _ => Err(format!("incomplete desktop list {output:?}")),
        }
    }

    fn is_current(&self, id: &str) -> bool {
        self.current.as_deref() == Some(id)
    }
}

pub(crate) fn read(cx: &mut Ctx<'_>) -> Result<Desktops, Failure> {
    let output = cx.helper(&["--virtual-desktops"])?;
    if !output.status.success() {
        return Err(Failure::host(format!(
            "cannot read the shell's desktops: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Desktops::parse(&String::from_utf8_lossy(&output.stdout)).map_err(Failure::host)
}

/// Sends `new`, `left`, `right` or `close` as the shell's Ctrl+Win shortcut.
pub(crate) fn shortcut(cx: &mut Ctx<'_>, which: &str) -> Result<(), Failure> {
    let output = cx.helper(&["--desktop-shortcut", which])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Failure::host(format!(
            "Windows refused the {which} desktop shortcut"
        )))
    }
}

/// Reads the desktops until `done` holds, for at most `SHELL_TIMEOUT`, and returns the last
/// reading with whether `done` held.
pub(crate) fn wait(
    cx: &mut Ctx<'_>,
    done: impl Fn(&Desktops) -> bool,
) -> Result<(Desktops, bool), Failure> {
    let deadline = Instant::now() + SHELL_TIMEOUT;
    loop {
        let desktops = read(cx)?;
        if done(&desktops) {
            return Ok((desktops, true));
        }
        if Instant::now() >= deadline {
            return Ok((desktops, false));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Adds a desktop with Ctrl+Win+D, which the shell also switches to. `Ok(Err(reason))` when the
/// shell recorded no new current desktop, so the runner cannot add one.
pub(crate) fn create(
    cx: &mut Ctx<'_>,
    start: &Desktops,
) -> Result<Result<String, String>, Failure> {
    shortcut(cx, "new")?;
    // A profile that never had a second desktop may record no desktop at all, so a new desktop
    // is a current desktop that was not listed, in a list longer than before and than one.
    let (now, created) = wait(cx, |now| {
        now.current.as_ref().is_some_and(|id| {
            !start.ids.contains(id)
                && now.ids.contains(id)
                && now.ids.len() > start.ids.len().max(1)
        })
    })?;
    match now.current {
        Some(id) if created => Ok(Ok(id)),
        _ => Ok(Err(format!(
            "Ctrl+Win+D added no desktop within {SHELL_TIMEOUT:?}: before {start:?}, after {now:?}"
        ))),
    }
}

/// Removes the desktop `created` with Ctrl+Win+F4 and checks the shell is back on the desktop
/// of `start` with `created` gone. Ctrl+Win+F4 closes whatever desktop is current, so it is sent
/// only once `created` is current; from `start`'s desktop that is one step right.
pub(crate) fn remove(cx: &mut Ctx<'_>, start: &Desktops, created: &str) -> Result<(), Failure> {
    if !read(cx)?.is_current(created) {
        shortcut(cx, "right")?;
        let (now, reached) = wait(cx, |now| now.is_current(created))?;
        if !reached {
            return Err(Failure::host(format!(
                "left the added desktop {created} in place: cannot make it current, now {now:?}"
            )));
        }
    }
    shortcut(cx, "close")?;
    let (now, restored) = wait(cx, |now| {
        !now.ids.iter().any(|id| id == created)
            && start.current.as_ref().is_none_or(|id| now.is_current(id))
    })?;
    if restored {
        Ok(())
    } else {
        Err(Failure::host(format!(
            "the runner did not return to {start:?} after closing {created}: now {now:?}"
        )))
    }
}

/// The `virtual-desktops` capability: the shell records its desktops, and a desktop can be added
/// and removed again. A missing piece is noted as the reason the task is blocked.
pub(crate) fn probe(cx: &mut Ctx<'_>) -> Result<bool, Failure> {
    let start = match read(cx) {
        Ok(start) => start,
        Err(failure) => {
            cx.note(format!("virtual-desktops blocked: {}", failure.message));
            return Ok(false);
        }
    };
    match create(cx, &start)? {
        Ok(created) => remove(cx, &start, &created).map(|()| true),
        Err(reason) => {
            cx.note(format!("virtual-desktops blocked: {reason}"));
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_output_parses_with_and_without_desktops() {
        assert_eq!(
            Desktops::parse("current A\r\nids A,B\r\n"),
            Ok(Desktops {
                current: Some("A".into()),
                ids: vec!["A".into(), "B".into()],
            })
        );
        assert_eq!(
            Desktops::parse("current none\nids\n"),
            Ok(Desktops {
                current: None,
                ids: Vec::new(),
            })
        );
        assert!(Desktops::parse("current A\n").is_err());
        assert!(Desktops::parse("current A\nids A\nother\n").is_err());
    }
}
