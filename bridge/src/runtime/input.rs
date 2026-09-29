//! The game's window procedure, subclassed.
//!
//! Every mouse message is forwarded to Blish over UDP, and clicks that land on one of Blish's
//! controls are kept from the game (see [`crate::clicks`]). The subclass also runs keybinds,
//! reports window size changes to Blish, and works around a few Wine input quirks.
//!
//! The procedure runs on the game's UI thread, so it only queues packets; a separate thread
//! does the socket I/O.

use std::{
    net::UdpSocket,
    panic::catch_unwind,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicIsize, AtomicU8, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetKeyState, SetFocus, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_NUMLOCK, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallWindowProcW, DefWindowProcW, GWLP_WNDPROC, GetWindowLongPtrW, SIZE_MINIMIZED,
            SetForegroundWindow, SetWindowLongPtrW, WM_ACTIVATE, WM_ACTIVATEAPP, WM_KEYDOWN,
            WM_KEYUP, WM_SETFOCUS, WM_SIZE, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDPROC,
        },
    },
};

use super::{link, sys::lock};
use crate::{
    Error, Result,
    clicks::{ClickRouter, MouseMessage},
    keybind::Keybind,
    protocol::{self, MousePacket},
};

/// The window procedure this one replaced, as returned by `GetWindowLongPtrW`.
static ORIGINAL: AtomicIsize = AtomicIsize::new(0);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

static SENDER: OnceLock<Sender<MousePacket>> = OnceLock::new();
static CLICKS: ClickRouter = ClickRouter::new();
static KEYBINDS: OnceLock<Vec<Keybind>> = OnceLock::new();

/// Last known NumLock state; see [`numlock_changed`].
static NUMLOCK: AtomicU8 = AtomicU8::new(0);
static LAST_ALT: Mutex<Option<Instant>> = Mutex::new(None);

/// NumLock presses this soon after Alt are ignored; see [`numlock_changed`].
const NUMLOCK_AFTER_ALT: Duration = Duration::from_millis(100);

/// Starts the thread that sends mouse packets to Blish.
pub fn start_sender() -> Result<()> {
    let socket = UdpSocket::bind(("127.0.0.1", 0))?;
    let (sender, packets) = mpsc::channel::<MousePacket>();

    SENDER
        .set(sender)
        .map_err(|_| Error::Unavailable("the mouse sender is already running".into()))?;

    thread::Builder::new()
        .name("blishhud-input".into())
        .spawn(move || {
            for packet in packets {
                // Nothing listening yet is normal while Blish starts up.
                let _ = socket.send_to(&packet.encode(), protocol::INPUT_ADDRESS);
            }
        })?;
    Ok(())
}

/// Subclasses the game's window.
pub fn install(window: HWND, keybinds: &[Keybind]) -> Result<()> {
    KEYBINDS.get_or_init(|| keybinds.to_vec());
    NUMLOCK.store(numlock_state(), Ordering::Relaxed);

    // Record the current procedure before replacing it: the game's thread may call ours the
    // moment it is installed, and must find something to forward to.
    // SAFETY: `window` is a live window of this process.
    ORIGINAL.store(
        unsafe { GetWindowLongPtrW(window, GWLP_WNDPROC) },
        Ordering::Relaxed,
    );

    // SAFETY: `window_proc` has the WNDPROC signature and lives for the rest of the process.
    let previous =
        unsafe { SetWindowLongPtrW(window, GWLP_WNDPROC, window_proc as *const () as isize) };
    if previous == 0 {
        return Err(windows::core::Error::from_win32().into());
    }

    ORIGINAL.store(previous, Ordering::Relaxed);
    WINDOW.store(window.0, Ordering::Relaxed);
    log::info!("subclassed the game window");
    Ok(())
}

/// Puts the original window procedure back.
pub fn uninstall() {
    let window = WINDOW.swap(0, Ordering::Relaxed);
    let original = ORIGINAL.load(Ordering::Relaxed);
    if window != 0 && original != 0 {
        // SAFETY: restores the procedure this module replaced.
        unsafe { SetWindowLongPtrW(HWND(window), GWLP_WNDPROC, original) };
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A panic must never unwind into the game; the message just passes through instead.
    let handled = catch_unwind(|| handle(window, message, wparam, lparam)).unwrap_or(None);

    match handled {
        Some(result) => result,
        None => forward(window, message, wparam, lparam),
    }
}

/// Handles a message, returning a result if it must not reach the game.
fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    const SWALLOW: Option<LRESULT> = Some(LRESULT(0));

    if let Some(mouse) = MouseMessage::from_wm(message) {
        send_mouse(message, wparam, lparam);
        // Only presses and the wheel consult Blish, so a stream of movement never touches the
        // shared header.
        let over_overlay =
            matches!(mouse, MouseMessage::Press(_) | MouseMessage::Wheel) && link::block_mouse();
        return CLICKS
            .should_block(mouse, over_overlay)
            .then_some(LRESULT(0));
    }

    let key = wparam.0;
    match message {
        WM_SYSKEYDOWN | WM_SYSKEYUP if key == usize::from(VK_NUMLOCK.0) => SWALLOW,

        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if key == usize::from(VK_MENU.0) {
                *lock(&LAST_ALT) = Some(Instant::now());
            }
            if key == usize::from(VK_NUMLOCK.0) && !numlock_changed() {
                return SWALLOW;
            }
            let pressed = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            if pressed && run_keybind(key as u32) {
                return SWALLOW;
            }
            None
        }

        WM_SETFOCUS => {
            bring_to_front(window);
            None
        }
        // For WM_ACTIVATE only the low word is the activation state; the high word is set when
        // the window is minimised, so the whole value is non-zero even while deactivating.
        WM_ACTIVATE if wparam.0 & 0xFFFF != 0 => {
            bring_to_front(window);
            None
        }
        WM_ACTIVATEAPP if wparam.0 != 0 => {
            bring_to_front(window);
            None
        }

        // Minimising reports a zero size, which Blish must not rebuild its textures at.
        WM_SIZE if wparam.0 != SIZE_MINIMIZED as usize => {
            let (width, height) = protocol::size_from_lparam(lparam.0);
            if width > 0 && height > 0 {
                link::set_game_size(width, height);
            }
            None
        }

        _ => None,
    }
}

fn forward(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let original = ORIGINAL.load(Ordering::Relaxed);
    if original == 0 {
        // SAFETY: the default procedure accepts any message for any window.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    // SAFETY: `original` came from GWLP_WNDPROC. It may be a handle rather than a function
    // pointer, which is exactly why it is only ever called through CallWindowProcW; the two
    // types share a representation.
    let original = unsafe { std::mem::transmute::<isize, WNDPROC>(original) };
    // SAFETY: forwards the message, unchanged, to the procedure we replaced.
    unsafe { CallWindowProcW(original, window, message, wparam, lparam) }
}

fn send_mouse(message: u32, wparam: WPARAM, lparam: LPARAM) {
    let (x, y) = protocol::point_from_lparam(lparam.0);
    if let Some(sender) = SENDER.get() {
        // Fails only once the sender thread has exited, when there is nobody left to tell.
        let _ = sender.send(MousePacket {
            message,
            x,
            y,
            data: wparam.0 as i32,
        });
    }
}

/// Runs the keybind for `key` under the current modifiers. Returns whether one matched.
fn run_keybind(key: u32) -> bool {
    let held = |key: VIRTUAL_KEY| {
        // SAFETY: no preconditions. A negative state means the key is down.
        unsafe { GetKeyState(i32::from(key.0)) < 0 }
    };
    let (ctrl, alt, shift) = (held(VK_CONTROL), held(VK_MENU), held(VK_SHIFT));

    let Some(bind) = KEYBINDS.get().and_then(|binds| {
        binds
            .iter()
            .find(|bind| bind.combo.matches(key, ctrl, alt, shift))
    }) else {
        return false;
    };

    super::run_action(bind.action);
    true
}

/// Whether a NumLock key message reflects a real change of the NumLock state.
///
/// Under Wine the game receives NumLock messages that do not correspond to the user pressing
/// it — when focus changes, and just after Alt — which toggles the game's own NumLock handling.
/// Only messages that actually changed the state, and did not closely follow Alt, count.
fn numlock_changed() -> bool {
    let after_alt = lock(&LAST_ALT).is_some_and(|alt| alt.elapsed() < NUMLOCK_AFTER_ALT);
    if after_alt {
        return false;
    }

    let state = numlock_state();
    NUMLOCK.swap(state, Ordering::Relaxed) != state
}

fn numlock_state() -> u8 {
    // SAFETY: no preconditions. The low bit is the toggle state.
    (unsafe { GetKeyState(i32::from(VK_NUMLOCK.0)) } & 1) as u8
}

/// Makes sure the game window really has focus once it is activated. Without this, the game
/// could stop accepting input after focus returned to it (upstream issues #12 and #22).
fn bring_to_front(window: HWND) {
    // SAFETY: `window` is the live game window.
    unsafe {
        let _ = SetForegroundWindow(window);
        SetFocus(window);
    }
}
