//! Renders Blish HUD inside Guild Wars 2.
//!
//! Blish HUD runs as its own hidden process and renders every frame into a pair of shared
//! D3D11 textures. This library runs inside the game, draws the most recent of those textures
//! on top of each frame the game presents, and feeds the game's mouse input back to Blish.
//!
//! # How it gets loaded
//!
//! The library is built as `d3d11.dll`. Placed next to `Gw2-64.exe`, it is loaded by the game in
//! place of the system `d3d11.dll` and forwards every D3D11 entry point to the real one — or to
//! another proxy such as arcdps first, when one is configured. See [`runtime::proxy`].
//!
//! It still works if injected by some other means; the exports are then simply never called.
//!
//! # Layout
//!
//! The pure modules have no platform dependencies and are unit-tested on any host:
//!
//! - [`protocol`] — the byte-level contract with Blish HUD's C# side
//! - [`clicks`] — which mouse messages must be kept from the game
//! - [`keybind`] — key combinations and the actions bound to them
//! - [`config`] — the `bridge.ini` file
//!
//! Everything that runs inside the game process lives in `runtime`, which only exists on
//! Windows.
//!
//! # Failure policy
//!
//! Nothing in here may take the game down. Errors are returned as values and logged, and every
//! entry point the game calls into catches panics (the release profile unwinds for this
//! reason). The worst case of a bug should be a missing overlay.

#![warn(
    clippy::undocumented_unsafe_blocks,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clicks;
pub mod config;
pub mod error;
pub mod keybind;
pub mod protocol;

#[cfg(windows)]
pub mod runtime;

pub use error::{Error, Result};

#[cfg(windows)]
mod entry {
    use std::ffi::c_void;

    use windows::Win32::{
        Foundation::{BOOL, HMODULE, TRUE},
        System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH},
    };

    use crate::runtime;

    /// The dll entry point.
    ///
    /// This runs under the loader lock, so it only records where the dll lives and starts the
    /// real work on a thread of its own. See [`runtime::on_attach`].
    #[unsafe(no_mangle)]
    extern "system" fn DllMain(module: HMODULE, reason: u32, reserved: *mut c_void) -> BOOL {
        match reason {
            DLL_PROCESS_ATTACH => runtime::on_attach(module),
            // A non-null `reserved` means the whole process is exiting rather than this dll
            // being unloaded, in which case there is nothing worth undoing.
            DLL_PROCESS_DETACH => runtime::on_detach(!reserved.is_null()),
            _ => {}
        }
        TRUE
    }
}
