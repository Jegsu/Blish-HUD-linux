//! Where the dll and its files live.
//!
//! Everything is anchored to the dll's own location, never the working directory: that belongs
//! to the game and varies with how it was launched.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use windows::Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleFileNameW};

static MODULE: OnceLock<HMODULE> = OnceLock::new();
static GAME_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Records this dll's module handle and location. Safe to call from `DllMain`.
pub fn init(module: HMODULE) {
    MODULE.get_or_init(|| module);
    GAME_DIR.get_or_init(|| module_dir(module).unwrap_or_else(|| PathBuf::from(".")));
}

/// This dll's module handle.
pub fn module() -> HMODULE {
    MODULE.get().copied().unwrap_or_default()
}

/// The directory the dll was loaded from — the game directory, when installed as `d3d11.dll`.
pub fn game_dir() -> &'static Path {
    GAME_DIR.get().map_or(Path::new("."), PathBuf::as_path)
}

/// Where the bridge keeps its config and logs.
pub fn data_dir() -> PathBuf {
    game_dir().join("addons").join("blishhud-bridge")
}

/// Resolves a path from the config: absolute paths are kept, relative ones are taken from the
/// game directory.
pub fn resolve(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        game_dir().join(path)
    }
}

fn module_dir(module: HMODULE) -> Option<PathBuf> {
    let mut buffer = vec![0u16; 1024];
    // SAFETY: the buffer is valid for writes of its full length.
    let len = unsafe { GetModuleFileNameW(module, &mut buffer) } as usize;

    // Zero is failure; a completely filled buffer means the path was truncated.
    if len == 0 || len >= buffer.len() {
        return None;
    }

    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
    path.parent().map(Path::to_owned)
}
