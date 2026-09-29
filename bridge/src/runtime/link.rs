//! The live connection to Blish HUD.
//!
//! A watcher thread notices Blish starting and stopping (through the mutex it holds while
//! running) and maps or unmaps the shared header accordingly. Everything else reads the header
//! on demand, so there is no cached state to go stale when Blish exits or restarts.

use std::{
    panic::catch_unwind,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, FALSE, HWND, RECT, TRUE},
        System::{
            Memory::{
                FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
                UnmapViewOfFile,
            },
            Threading::{CreateEventW, OpenMutexW, SYNCHRONIZATION_ACCESS_RIGHTS, SetEvent},
        },
        UI::WindowsAndMessaging::GetClientRect,
    },
    core::PCWSTR,
};

use super::sys::{OwnedHandle, lock, to_wide};
use crate::{
    Result,
    protocol::{self, DIMENSIONS_OFFSET, HEADER_SIZE, Header},
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `SYNCHRONIZE` access: enough to learn whether the alive mutex exists.
const SYNCHRONIZE: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000);

/// The mapped header while Blish is running.
static MAPPING: Mutex<Option<Mapping>> = Mutex::new(None);

/// The game's client size, packed as `width << 32 | height`; zero until known.
static GAME_SIZE: AtomicU64 = AtomicU64::new(0);

/// Whether the current connection has been told the game's size.
static SIZE_SENT: AtomicBool = AtomicBool::new(false);

/// How long Blish may run without its header opening before that is reported. Blish creates the
/// header on its first frame, which can take a while on a cold start.
const CONNECT_GRACE: Duration = Duration::from_secs(15);

/// When Blish was first seen running without its header opening, while that lasts.
static WAITING_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether the current wait has already been reported as taking too long.
static WAIT_REPORTED: AtomicBool = AtomicBool::new(false);

/// Starts the thread that follows Blish HUD starting and stopping.
pub fn start_watcher() -> Result<()> {
    thread::Builder::new()
        .name("blishhud-link".into())
        .spawn(|| {
            loop {
                // A panic here must not end the watcher, or the bridge would silently stop
                // following Blish.
                if catch_unwind(poll).is_err() {
                    log::error!("the link watcher panicked; retrying");
                }
                thread::sleep(POLL_INTERVAL);
            }
        })?;
    Ok(())
}

/// Records the game's window, taking its current size as the size to report to Blish.
pub fn set_game_window(window: HWND) {
    let mut rect = RECT::default();
    // SAFETY: `window` is a live window and the out pointer is valid.
    if unsafe { GetClientRect(window, &mut rect) }.is_ok() {
        let size = (rect.right - rect.left, rect.bottom - rect.top);
        if let (Ok(width), Ok(height)) = (u32::try_from(size.0), u32::try_from(size.1)) {
            set_game_size(width, height);
        }
    }
}

/// Reports a new game size to Blish, which rebuilds its textures to match.
pub fn set_game_size(width: u32, height: u32) {
    GAME_SIZE.store(
        u64::from(width) << 32 | u64::from(height),
        Ordering::Relaxed,
    );

    if let Some(mapping) = lock(&MAPPING).as_ref() {
        mapping.write_size(width, height);
        SIZE_SENT.store(true, Ordering::Relaxed);
    }
}

/// The current header, or `None` while Blish HUD is not running.
pub fn header() -> Option<Header> {
    lock(&MAPPING).as_ref().map(Mapping::read)
}

/// Whether the cursor is over one of Blish's controls. Always false without Blish.
pub fn block_mouse() -> bool {
    header().is_some_and(|header| header.block_mouse)
}

/// Whether Blish HUD is running.
pub fn blish_alive() -> bool {
    let name = to_wide(protocol::ALIVE_MUTEX_NAME);
    // SAFETY: `name` is a null-terminated UTF-16 string that outlives the call.
    match unsafe { OpenMutexW(SYNCHRONIZE, FALSE, PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            // SAFETY: the handle was just opened and is owned by nothing else.
            drop(unsafe { OwnedHandle::new(handle) });
            true
        }
        // The mutex exists but may not be opened by us: Blish is still running.
        Err(error) => error.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

fn poll() {
    let alive = blish_alive();
    let mut mapping = lock(&MAPPING);

    match (alive, mapping.is_some()) {
        // Blish creates the header on its first frame, so opening can fail for a while after
        // it starts; the next poll simply tries again.
        (true, false) => match Mapping::open() {
            Ok(opened) => {
                log::info!("connected to Blish HUD");
                *mapping = Some(opened);
                SIZE_SENT.store(false, Ordering::Relaxed);
                *lock(&WAITING_SINCE) = None;
                WAIT_REPORTED.store(false, Ordering::Relaxed);
            }
            Err(error) => report_slow_connect(&error),
        },
        (false, true) => {
            log::info!("Blish HUD exited");
            *mapping = None;
        }
        (false, false) => *lock(&WAITING_SINCE) = None,
        (true, true) => {}
    }
    drop(mapping);

    // Reports a Blish that died on its own, which would otherwise look the same as one that
    // was never started.
    super::launcher::report_exit();

    let mapping = lock(&MAPPING);

    // Blish only builds its textures once told the game's size.
    if let Some(mapping) = mapping.as_ref() {
        let size = GAME_SIZE.load(Ordering::Relaxed);
        if size != 0 && !SIZE_SENT.swap(true, Ordering::Relaxed) {
            mapping.write_size((size >> 32) as u32, size as u32);
        }
    }
}

/// Logs when Blish is running but its header still cannot be opened. Briefly, that is normal;
/// for long, it usually means a Blish HUD build that does not speak this bridge's protocol.
fn report_slow_connect(error: &crate::Error) {
    let mut since = lock(&WAITING_SINCE);
    let Some(started) = *since else {
        log::info!("Blish HUD is running; waiting for its shared memory");
        *since = Some(Instant::now());
        return;
    };

    if started.elapsed() >= CONNECT_GRACE && !WAIT_REPORTED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "Blish HUD has been running for {}s, but its shared memory still cannot be opened \
             ({error}). This usually means the Blish HUD build does not match this bridge: it \
             must be built from the same repository.",
            CONNECT_GRACE.as_secs()
        );
    }
}

/// The manual-reset event signalled after writing a new size.
fn resize_event() -> Option<&'static OwnedHandle> {
    static EVENT: OnceLock<Option<OwnedHandle>> = OnceLock::new();

    EVENT
        .get_or_init(|| {
            let name = to_wide(protocol::RESIZE_EVENT_NAME);
            // SAFETY: `name` is a null-terminated UTF-16 string that outlives the call.
            let event = unsafe { CreateEventW(None, TRUE, FALSE, PCWSTR(name.as_ptr())) };
            event
                .inspect_err(|error| log::error!("could not create the resize event: {error}"))
                .ok()
                // SAFETY: the event was just created and is owned by nothing else.
                .map(|handle| unsafe { OwnedHandle::new(handle) })
        })
        .as_ref()
}

/// The shared header, mapped into this process.
struct Mapping {
    _handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the view is ordinary shared memory, valid from any thread until it is unmapped in
// `drop`, and every access to it goes through the `MAPPING` mutex.
unsafe impl Send for Mapping {}

impl Mapping {
    fn open() -> Result<Self> {
        let name = to_wide(protocol::HEADER_MAPPING_NAME);
        // SAFETY: `name` is a null-terminated UTF-16 string that outlives the call.
        let handle =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, FALSE, PCWSTR(name.as_ptr())) }?;
        // SAFETY: the handle was just opened and is owned by nothing else.
        let handle = unsafe { OwnedHandle::new(handle) };

        // SAFETY: maps exactly the header's size from a mapping Blish created at that size.
        let view = unsafe { MapViewOfFile(handle.raw(), FILE_MAP_ALL_ACCESS, 0, 0, HEADER_SIZE) };
        if view.Value.is_null() {
            return Err(windows::core::Error::from_win32().into());
        }

        Ok(Self {
            _handle: handle,
            view,
        })
    }

    fn base(&self) -> *mut u8 {
        self.view.Value.cast()
    }

    fn read(&self) -> Header {
        let mut bytes = [0; HEADER_SIZE];
        for (offset, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: the view is `HEADER_SIZE` bytes long. Volatile, because Blish writes this
            // memory from another process at any time.
            *byte = unsafe { self.base().add(offset).read_volatile() };
        }
        Header::decode(&bytes)
    }

    fn write_size(&self, width: u32, height: u32) {
        for (offset, byte) in protocol::encode_dimensions(width, height)
            .into_iter()
            .enumerate()
        {
            // SAFETY: the dimensions lie within the `HEADER_SIZE`-byte view. Volatile, because
            // the memory is shared with another process.
            unsafe {
                self.base()
                    .add(DIMENSIONS_OFFSET + offset)
                    .write_volatile(byte)
            };
        }

        if let Some(event) = resize_event() {
            // SAFETY: the event handle is valid for the life of the process.
            if let Err(error) = unsafe { SetEvent(event.raw()) } {
                log::warn!("could not signal the resize event: {error}");
            }
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the view was mapped in `open` and is unmapped exactly once, here.
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}
