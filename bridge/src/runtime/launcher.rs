//! Starts Blish HUD with the game, and makes sure it stops with it.
//!
//! Blish runs inside a job object set to kill its processes when the job's last handle closes.
//! The bridge holds that handle for the rest of the game's life, so however the game exits —
//! even killed outright — Blish goes with it. Left running, Blish would keep the Wine prefix,
//! and with it the game's Steam session, alive.

use std::{
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    ptr,
    sync::{Mutex, OnceLock},
};

use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::CREATE_NO_WINDOW,
        },
    },
    core::PCWSTR,
};

use super::{
    link, paths,
    sys::{OwnedHandle, lock},
};
use crate::{Error, Result, config::Config};

/// The Blish HUD process the bridge started, if any.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Starts Blish HUD, unless it is already running.
pub fn start(config: &Config) {
    if link::blish_alive() {
        log::info!("Blish HUD is already running");
        return;
    }
    if let Err(error) = spawn(config) {
        log::error!("could not start Blish HUD: {error}");
    }
}

/// Stops the Blish HUD the bridge started, and starts it again.
pub fn restart(config: &Config) {
    let mut child = lock(&CHILD);
    match child.take() {
        Some(mut running) => {
            // Waiting for it to be gone keeps the new instance from seeing the old one.
            let _ = running.kill();
            let _ = running.wait();
        }
        None if link::blish_alive() => {
            log::warn!("Blish HUD was not started by the bridge, so it cannot restart it");
            return;
        }
        None => {}
    }
    drop(child);

    if let Err(error) = spawn(config) {
        log::error!("could not restart Blish HUD: {error}");
    }
}

/// Logs, once, when the Blish HUD the bridge started has exited on its own.
pub fn report_exit() {
    let mut child = lock(&CHILD);
    let Some(running) = child.as_mut() else {
        return;
    };

    match running.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            log::error!("Blish HUD exited on its own ({status}); its own log may say why");
            *child = None;
        }
        Err(error) => {
            log::warn!("could not check on Blish HUD: {error}");
            *child = None;
        }
    }
}

fn spawn(config: &Config) -> Result<()> {
    let exe = paths::resolve(&config.blish_path);
    if !exe.is_file() {
        return Err(Error::Unavailable(format!(
            "Blish HUD not found at {}",
            exe.display()
        )));
    }

    let child = Command::new(&exe)
        .current_dir(exe.parent().unwrap_or(paths::game_dir()))
        .creation_flags(CREATE_NO_WINDOW.0)
        // Not piped: nothing would read the pipes, and a full one blocks Blish's logging.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Some(job) = job() {
        // SAFETY: both handles are valid; the child's is borrowed from `child`, which outlives
        // the call.
        let assigned =
            unsafe { AssignProcessToJobObject(job.raw(), HANDLE(child.as_raw_handle() as isize)) };
        if let Err(error) = assigned {
            log::warn!("Blish HUD will outlive the game: {error}");
        }
    }

    log::info!("started {}", exe.display());
    *lock(&CHILD) = Some(child);
    Ok(())
}

/// The kill-on-close job Blish runs in, created on first use.
fn job() -> Option<&'static OwnedHandle> {
    static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();

    JOB.get_or_init(|| {
        create_kill_on_close_job()
            .inspect_err(|error| log::warn!("Blish HUD will outlive the game: {error}"))
            .ok()
    })
    .as_ref()
}

fn create_kill_on_close_job() -> Result<OwnedHandle> {
    // SAFETY: creates an unnamed job with default security, owned from here on.
    let job = unsafe { OwnedHandle::new(CreateJobObjectW(None, PCWSTR::null())?) };

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    // SAFETY: `limits` is the structure this information class expects, and outlives the call.
    unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            ptr::from_ref(&limits).cast(),
            size_of_val(&limits) as u32,
        )
    }?;

    Ok(job)
}
