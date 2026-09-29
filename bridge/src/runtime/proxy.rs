//! Stands in for `d3d11.dll`.
//!
//! Windows searches the application directory before `System32`, and `d3d11.dll` is not one of
//! the "known DLLs" exempt from that, so a `d3d11.dll` next to `Gw2-64.exe` is loaded in place
//! of the real one. That is how the bridge gets into the game without an injector.
//!
//! Every D3D11 entry point the game calls is forwarded down a chain:
//!
//! ```text
//! game -> this dll -> chainloaded proxy (e.g. arcdps) -> system d3d11.dll
//! ```
//!
//! The chainloaded dll is optional (see [`Config::chainload`](crate::config::Config)). arcdps
//! is itself a `d3d11.dll` proxy that forwards to the system copy, so it slots in unchanged: it
//! sees exactly the calls it would if it had been loaded as `d3d11.dll` directly. Any export it
//! does not provide falls back to the system `d3d11.dll`.
//!
//! A proxy not named `d3d11.dll` may still load "the real one" by that bare name, and gets this
//! dll back — arcdps does exactly that when renamed to `arcdps.dll`. Its calls then come back in
//! here while the chain is still being walked; those go straight to the system `d3d11.dll`
//! rather than round the loop forever.
//!
//! The chain is resolved on the game's first D3D11 call rather than in `DllMain`, because
//! loading other dlls under the loader lock can deadlock.

use std::{cell::Cell, ffi::c_void, path::Path, sync::OnceLock};

use windows::{
    Win32::{
        Foundation::{E_FAIL, HMODULE},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryW},
            SystemInformation::GetSystemDirectoryW,
        },
    },
    core::{HRESULT, PCSTR, PCWSTR},
};

use super::{paths, sys::to_wide};

/// Declares the forwarded exports.
///
/// For each entry this generates an exported function with that exact name and signature, and
/// a slot in [`Exports`] holding the next implementation down the chain. The set matches what
/// DXVK exports — which is everything the game can need, since the game runs on DXVK.
macro_rules! d3d11_exports {
    ($( $name:ident( $($arg:ident: $ty:ty),* $(,)? ); )*) => {
        /// The next implementation of each export, where one was found.
        #[allow(non_snake_case)]
        #[derive(Default)]
        struct Exports {
            $( $name: Option<unsafe extern "system" fn($($ty),*) -> HRESULT>, )*
        }

        impl Exports {
            /// Looks every export up in `modules`, taking the first module that provides it.
            fn resolve(modules: &[HMODULE]) -> Self {
                Self {
                    $(
                        $name: find_export(modules, concat!(stringify!($name), "\0")).map(|proc| {
                            // SAFETY: the pointer was exported under this name, and the D3D11
                            // ABI fixes the signature for that name.
                            unsafe {
                                std::mem::transmute::<
                                    unsafe extern "system" fn() -> isize,
                                    unsafe extern "system" fn($($ty),*) -> HRESULT,
                                >(proc)
                            }
                        }),
                    )*
                }
            }

            /// Names of the exports no module provided.
            fn missing(&self) -> Vec<&'static str> {
                let mut missing = Vec::new();
                $( if self.$name.is_none() { missing.push(stringify!($name)); } )*
                missing
            }
        }

        $(
            #[doc = concat!("Forwards `", stringify!($name), "` to the next d3d11 in the chain.")]
            ///
            /// # Safety
            ///
            /// Same contract as the D3D11 function of the same name; the arguments are passed
            /// through untouched.
            #[allow(non_snake_case)]
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn $name($($arg: $ty),*) -> HRESULT {
                let reentered = FORWARDING.get();
                let exports = if reentered { system_exports() } else { chain() };
                let Some(next) = exports.$name else {
                    return E_FAIL;
                };

                FORWARDING.set(true);
                // SAFETY: forwarded unchanged to an implementation of this very function.
                let result = unsafe { next($($arg),*) };
                FORWARDING.set(reentered);
                result
            }
        )*
    };
}

d3d11_exports! {
    D3D11CreateDevice(
        adapter: *mut c_void,
        driver_type: i32,
        software: *mut c_void,
        flags: u32,
        feature_levels: *const i32,
        num_feature_levels: u32,
        sdk_version: u32,
        device: *mut *mut c_void,
        feature_level: *mut i32,
        context: *mut *mut c_void,
    );
    D3D11CreateDeviceAndSwapChain(
        adapter: *mut c_void,
        driver_type: i32,
        software: *mut c_void,
        flags: u32,
        feature_levels: *const i32,
        num_feature_levels: u32,
        sdk_version: u32,
        swapchain_desc: *const c_void,
        swapchain: *mut *mut c_void,
        device: *mut *mut c_void,
        feature_level: *mut i32,
        context: *mut *mut c_void,
    );
    D3D11CoreCreateDevice(
        factory: *mut c_void,
        adapter: *mut c_void,
        flags: u32,
        feature_levels: *const i32,
        num_feature_levels: u32,
        device: *mut *mut c_void,
    );
    D3D11On12CreateDevice(
        device: *mut c_void,
        flags: u32,
        feature_levels: *const i32,
        num_feature_levels: u32,
        command_queues: *const *mut c_void,
        num_queues: u32,
        node_mask: u32,
        device_out: *mut *mut c_void,
        context: *mut *mut c_void,
        chosen_feature_level: *mut i32,
    );
}

/// `D3D11CreateDeviceAndSwapChain`'s signature, for callers inside the bridge.
pub(super) type CreateDeviceAndSwapChain = unsafe extern "system" fn(
    *mut c_void,
    i32,
    *mut c_void,
    u32,
    *const i32,
    u32,
    u32,
    *const c_void,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> HRESULT;

/// `D3D11CreateDeviceAndSwapChain` from the *system* `d3d11.dll`, bypassing any chainloaded
/// proxy. The bridge uses this for its own throwaway device, which a proxy such as arcdps
/// should never mistake for the game's.
pub(super) fn system_create_device_and_swapchain() -> Option<CreateDeviceAndSwapChain> {
    let proc = find_export(&[system_d3d11()?], "D3D11CreateDeviceAndSwapChain\0")?;
    // SAFETY: the system export of this name has exactly this signature.
    Some(unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateDeviceAndSwapChain>(proc)
    })
}

thread_local! {
    /// Set on a thread while one of its D3D11 calls is being forwarded down the chain.
    static FORWARDING: Cell<bool> = const { Cell::new(false) };
}

/// The chain, resolved on first use.
fn chain() -> &'static Exports {
    static CHAIN: OnceLock<Exports> = OnceLock::new();

    CHAIN.get_or_init(|| {
        let Some(system) = system_d3d11() else {
            log::error!("the system d3d11.dll could not be loaded; D3D11 is unavailable");
            return Exports::default();
        };

        let mut modules = Vec::with_capacity(2);
        if let Some(chainload) = &super::config().chainload {
            modules.extend(load_chainload(&paths::resolve(chainload)));
        }
        modules.push(system);

        let exports = Exports::resolve(&modules);
        let missing = exports.missing();
        if !missing.is_empty() {
            log::warn!("no implementation found for {}", missing.join(", "));
        }
        exports
    })
}

/// The system `d3d11.dll`'s exports alone, for calls a chainloaded proxy makes back into this
/// dll.
fn system_exports() -> &'static Exports {
    static SYSTEM_EXPORTS: OnceLock<Exports> = OnceLock::new();

    SYSTEM_EXPORTS.get_or_init(|| {
        log::info!("the chainloaded dll called back into this one; sending it to the system d3d11");
        system_d3d11().map_or_else(Exports::default, |system| Exports::resolve(&[system]))
    })
}

/// The real `d3d11.dll` from the system directory. Loaded by full path, so the loader cannot
/// hand back this dll, which shares the name.
fn system_d3d11() -> Option<HMODULE> {
    static SYSTEM: OnceLock<Option<HMODULE>> = OnceLock::new();

    *SYSTEM.get_or_init(|| {
        let mut buffer = vec![0u16; 512];
        // SAFETY: the buffer is valid for writes of its full length.
        let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 || len >= buffer.len() {
            log::error!("could not determine the system directory");
            return None;
        }

        let path = Path::new(&String::from_utf16_lossy(&buffer[..len])).join("d3d11.dll");
        load_library(&path)
            .inspect_err(|error| log::error!("could not load {}: {error}", path.display()))
            .ok()
    })
}

/// Loads the configured chainload dll, if it exists.
fn load_chainload(path: &Path) -> Option<HMODULE> {
    if !path.is_file() {
        log::info!("no chainload dll at {}", path.display());
        return None;
    }

    let module = load_library(path)
        .inspect_err(|error| log::error!("could not load {}: {error}", path.display()))
        .ok()?;

    // Chainloading ourselves would forward every call back into this dll, forever.
    if module == paths::module() {
        log::error!(
            "chainload {} is this dll; ignoring it to avoid infinite recursion",
            path.display()
        );
        return None;
    }

    log::info!("chainloaded {}", path.display());
    Some(module)
}

fn load_library(path: &Path) -> windows::core::Result<HMODULE> {
    let wide = to_wide(path);
    // SAFETY: `wide` is a null-terminated UTF-16 path that outlives the call.
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
}

/// Looks up `name` (which must end in a NUL) in each module in turn.
fn find_export(
    modules: &[HMODULE],
    name: &'static str,
) -> Option<unsafe extern "system" fn() -> isize> {
    debug_assert!(name.ends_with('\0'));
    modules.iter().find_map(|&module| {
        // SAFETY: `module` is a loaded module and `name` is a null-terminated static string.
        unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }
    })
}
