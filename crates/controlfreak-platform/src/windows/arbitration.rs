#![allow(unsafe_code)]

use std::{
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0},
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    },
    core::w,
};

pub struct DesktopArbitrator {
    sender: mpsc::Sender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

enum Command {
    TryAcquire(mpsc::SyncSender<Result<(), String>>),
    Release,
    Shutdown,
}

impl DesktopArbitrator {
    pub(crate) fn start() -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("controlfreak-desktop-owner".to_owned())
            .spawn(move || owner_loop(&receiver, &ready_tx))
            .map_err(|error| format!("desktop arbitration thread could not start: {error}"))?;
        ready_rx.recv().map_err(|error| {
            format!("desktop arbitration thread stopped during startup: {error}")
        })??;
        Ok(Self {
            sender,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub fn try_acquire(&self) -> Result<bool, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.sender
            .send(Command::TryAcquire(reply_tx))
            .map_err(|error| format!("desktop arbitration thread stopped: {error}"))?;
        reply_rx
            .recv()
            .map_err(|error| format!("desktop arbitration reply was lost: {error}"))?
            .map(|()| true)
            .or_else(|reason| {
                if reason == "busy" {
                    Ok(false)
                } else {
                    Err(reason)
                }
            })
    }

    pub fn release(&self) {
        let _ = self.sender.send(Command::Release);
    }
}

impl Drop for DesktopArbitrator {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}

fn owner_loop(receiver: &mpsc::Receiver<Command>, ready: &mpsc::SyncSender<Result<(), String>>) {
    // SAFETY: The name is a static, valid UTF-16 string and the handle is closed below.
    let mutex = match unsafe { CreateMutexW(None, false, w!("Local\\ControlFreakDesktopControl")) }
    {
        Ok(mutex) => mutex,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "desktop arbitration mutex could not open: {error}"
            )));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut owned = false;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::TryAcquire(reply) => {
                if owned {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                // SAFETY: `mutex` remains valid for the whole owner-loop lifetime.
                let outcome = unsafe { WaitForSingleObject(mutex, 0) };
                if outcome == WAIT_OBJECT_0 || outcome == WAIT_ABANDONED {
                    owned = true;
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err("busy".to_owned()));
                }
            }
            Command::Release if owned => {
                // SAFETY: The same thread that acquired the mutex releases it exactly once.
                let _ = unsafe { ReleaseMutex(mutex) };
                owned = false;
            }
            Command::Release => {}
            Command::Shutdown => break,
        }
    }
    if owned {
        // SAFETY: The same thread that acquired the mutex releases it during shutdown.
        let _ = unsafe { ReleaseMutex(mutex) };
    }
    // SAFETY: The mutex handle is valid and no longer used.
    let _ = unsafe { CloseHandle(mutex) };
}
