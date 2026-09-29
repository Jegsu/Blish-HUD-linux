//! The detour on `IDXGISwapChain::Present`, where the overlay is drawn.
//!
//! Every swapchain from the same DXGI implementation shares its `Present`, so the bridge
//! learns the address from a throwaway swapchain of its own and detours the function itself.
//! That catches the game's swapchain without needing to know how the game created it.

use std::{
    ffi::c_void,
    ptr::{self, null_mut},
    sync::OnceLock,
};

use retour::GenericDetour;
use windows::{
    Win32::{
        Foundation::{E_FAIL, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::D3D11_SDK_VERSION,
            Dxgi::{
                Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_SAMPLE_DESC},
                DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
                IDXGISwapChain,
            },
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, UnregisterClassW,
            WINDOW_EX_STYLE, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
        },
    },
    core::{HRESULT, Interface, PCWSTR, w},
};

use super::{paths, proxy, renderer};
use crate::{Error, Result};

type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT;

static PRESENT: OnceLock<GenericDetour<PresentFn>> = OnceLock::new();

/// Detours `Present` so the overlay is drawn on every frame.
pub fn install() -> Result<()> {
    let target = find_present()?;

    // SAFETY: `target` is a live `Present` implementation with exactly `PresentFn`'s
    // signature, which `present` shares.
    let detour = unsafe { GenericDetour::<PresentFn>::new(target, present) }?;

    // Stored before enabling, since `present` calls the original through this static.
    PRESENT
        .set(detour)
        .map_err(|_| Error::Unavailable("the Present hook is already installed".into()))?;
    let detour = PRESENT
        .get()
        .ok_or_else(|| Error::Unavailable("the Present hook was not stored".into()))?;

    // SAFETY: the detour lives in a static for the rest of the process, so the patched
    // function never jumps into freed memory.
    unsafe { detour.enable() }?;
    log::info!("hooked Present");
    Ok(())
}

/// Restores the original `Present`, for when the dll is unloaded while the game keeps running.
pub fn uninstall() {
    if let Some(detour) = PRESENT.get() {
        // SAFETY: restores the original bytes of a function this module patched.
        if let Err(error) = unsafe { detour.disable() } {
            log::error!("could not unhook Present: {error}");
        }
    }
}

unsafe extern "system" fn present(
    swapchain: *mut c_void,
    sync_interval: u32,
    flags: u32,
) -> HRESULT {
    // SAFETY: the game passes its live swapchain as `this`; it is only borrowed for this call.
    if let Some(swapchain) = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) } {
        renderer::draw(swapchain);
    }

    match PRESENT.get() {
        // SAFETY: calls the original `Present` with the game's own arguments.
        Some(detour) => unsafe { detour.call(swapchain, sync_interval, flags) },
        // Unreachable: the detour is only enabled once stored.
        None => E_FAIL,
    }
}

/// Finds `Present` by creating a throwaway device and swapchain and reading its vtable.
///
/// The device comes from the *system* `d3d11.dll`, bypassing any chainloaded proxy, which must
/// not mistake it for the game's.
fn find_present() -> Result<PresentFn> {
    let create = proxy::system_create_device_and_swapchain()
        .ok_or_else(|| Error::Unavailable("the system D3D11CreateDeviceAndSwapChain".into()))?;
    let window = ProbeWindow::create()?;

    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 1,
        OutputWindow: window.handle,
        Windowed: TRUE,
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        Flags: 0,
    };
    let feature_levels = [D3D_FEATURE_LEVEL_11_0.0, D3D_FEATURE_LEVEL_10_0.0];
    let mut swapchain = null_mut();

    // SAFETY: the arguments follow D3D11CreateDeviceAndSwapChain's contract: `desc` and
    // `feature_levels` outlive the call, and the optional device, feature level and context
    // outputs are left null.
    unsafe {
        create(
            null_mut(),
            D3D_DRIVER_TYPE_HARDWARE.0,
            null_mut(),
            0,
            feature_levels.as_ptr(),
            feature_levels.len() as u32,
            D3D11_SDK_VERSION,
            ptr::from_ref(&desc).cast(),
            &mut swapchain,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    }
    .ok()?;

    // SAFETY: on success, `swapchain` holds a reference we own; `from_raw` takes it over and
    // releases it (and with it the device) when dropped at the end of this function.
    let swapchain = unsafe { IDXGISwapChain::from_raw(swapchain) };
    Ok(swapchain.vtable().Present)
}

/// A hidden window for the throwaway swapchain, destroyed with its class when dropped.
struct ProbeWindow {
    handle: HWND,
    instance: HINSTANCE,
}

impl ProbeWindow {
    const CLASS: PCWSTR = w!("blishhud_bridge_probe");

    fn create() -> Result<Self> {
        let instance = HINSTANCE(paths::module().0);
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(probe_window_proc),
            hInstance: instance,
            lpszClassName: Self::CLASS,
            ..Default::default()
        };

        // SAFETY: `class` is fully initialised and its strings are static.
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(windows::core::Error::from_win32().into());
        }

        // SAFETY: the class was just registered. The window is never shown.
        let handle = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                Self::CLASS,
                Self::CLASS,
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                instance,
                None,
            )
        };
        if handle.0 == 0 {
            let error = windows::core::Error::from_win32();
            // SAFETY: unregisters the class registered above, which has no windows.
            let _ = unsafe { UnregisterClassW(Self::CLASS, instance) };
            return Err(error.into());
        }

        Ok(Self { handle, instance })
    }
}

impl Drop for ProbeWindow {
    fn drop(&mut self) {
        // SAFETY: destroys the window this value created, on the thread that created it, then
        // unregisters its now windowless class.
        unsafe {
            let _ = DestroyWindow(self.handle);
            let _ = UnregisterClassW(Self::CLASS, self.instance);
        }
    }
}

unsafe extern "system" fn probe_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwards a message for a window of this class to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
