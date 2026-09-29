//! Finding the game's window.

use std::{thread, time::Duration};

use windows::Win32::{
    Foundation::{BOOL, FALSE, HWND, LPARAM, TRUE},
    System::Threading::GetCurrentProcessId,
    UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId},
};

/// Window class of the game's DirectX 11 window.
///
/// Matching on the class matters: loaded as `d3d11.dll`, the bridge starts before the game's
/// launcher, which is another top-level window of the same process (class `ArenaNet`).
const GAME_WINDOW_CLASS: &str = "ArenaNet_Gr_Window_Class";

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Blocks until the game's window exists, however long that takes — the player may sit in the
/// launcher for a while first.
pub fn wait_for_game_window() -> HWND {
    loop {
        if let Some(window) = find_game_window() {
            return window;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// The game's window, if it has been created yet.
pub fn find_game_window() -> Option<HWND> {
    let mut found: Option<HWND> = None;
    // SAFETY: the callback only runs during this call, while `found` is alive, and receives a
    // pointer to it through `lparam`. An early stop reports an error, which is expected.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(std::ptr::from_mut(&mut found) as isize)) };
    found
}

unsafe extern "system" fn visit(window: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Option<HWND>` passed by `find_game_window`, which outlives
    // the enumeration.
    let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };

    let mut process_id = 0;
    // SAFETY: `window` comes from EnumWindows, and the out pointer is valid.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    // SAFETY: no preconditions.
    if process_id != unsafe { GetCurrentProcessId() } {
        return TRUE;
    }

    let mut class = [0u16; 64];
    // SAFETY: the buffer is valid for writes of its full length.
    let len = unsafe { GetClassNameW(window, &mut class) };
    let len = usize::try_from(len).unwrap_or(0);

    if String::from_utf16_lossy(&class[..len]) == GAME_WINDOW_CLASS {
        *found = Some(window);
        return FALSE;
    }
    TRUE
}
