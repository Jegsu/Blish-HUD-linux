//! Everything that runs inside the game process.
//!
//! # Startup
//!
//! [`on_attach`] runs under the loader lock, so it only records where the dll lives and hands
//! off to a startup thread, which:
//!
//! 1. loads the config (which also starts logging)
//! 2. starts the input sender and the watcher that follows Blish HUD starting and stopping
//! 3. launches Blish HUD, if configured
//! 4. waits for the game window — the player may be in the launcher for a while
//! 5. hooks `Present` and subclasses the window
//!
//! The D3D11 exports in [`proxy`] can be called by the game at any point, possibly before the
//! startup thread has done anything; they load the config themselves when needed.

mod hook;
mod input;
mod launcher;
mod link;
mod logging;
mod paths;
pub mod proxy;
mod renderer;
mod sys;
mod window;

use std::{fs, io, panic::catch_unwind, sync::OnceLock, thread};

use windows::Win32::Foundation::HMODULE;

use crate::{
    Result,
    config::{self, Config},
    keybind::Action,
};

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Called from `DllMain` when the dll is loaded.
pub fn on_attach(module: HMODULE) {
    paths::init(module);

    // Nothing else may happen under the loader lock. The thread only starts running once the
    // lock is released.
    let spawned = thread::Builder::new()
        .name("blishhud-startup".into())
        .spawn(|| match catch_unwind(start) {
            Ok(Ok(())) => log::info!("bridge ready"),
            Ok(Err(error)) => log::error!("bridge startup failed: {error}"),
            Err(_) => log::error!("bridge startup panicked"),
        });
    // There is no way to report this this early; the game simply runs without the overlay.
    drop(spawned);
}

/// Called from `DllMain` when the dll is unloaded. When the whole process is exiting there is
/// nothing worth undoing.
pub fn on_detach(process_exiting: bool) {
    if !process_exiting {
        input::uninstall();
        hook::uninstall();
    }
}

/// The bridge's config. The first call loads it, starting logging first.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(load_config)
}

fn start() -> Result<()> {
    let config = config();

    input::start_sender()?;
    link::start_watcher()?;
    if config.launch_blish {
        launcher::start(config);
    }

    log::info!("waiting for the game window");
    let window = window::wait_for_game_window();
    link::set_game_window(window);
    renderer::set_game_window(window);

    hook::install()?;
    input::install(window, &config.keybinds)
}

fn load_config() -> Config {
    let dir = paths::data_dir();
    logging::init(&dir);
    log::info!(
        "Blish HUD bridge {} loaded from {}",
        env!("CARGO_PKG_VERSION"),
        paths::game_dir().display()
    );

    let path = dir.join(config::FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // First run: write the defaults out, so the settings are discoverable.
            match fs::write(&path, config::TEMPLATE) {
                Ok(()) => log::info!("wrote default settings to {}", path.display()),
                Err(error) => log::warn!("could not write {}: {error}", path.display()),
            }
            config::TEMPLATE.to_owned()
        }
        Err(error) => {
            log::warn!("could not read {}, using defaults: {error}", path.display());
            String::new()
        }
    };

    let parsed = config::parse(&text);
    for warning in &parsed.warnings {
        log::warn!("{}: {warning}", path.display());
    }
    parsed.config
}

/// Runs a keybind's action. Called from the game's window procedure, so anything slow goes to
/// a thread of its own.
fn run_action(action: Action) {
    log::info!("keybind: {action}");

    match action {
        Action::DumpState => {
            log::info!("Blish HUD running: {}", link::blish_alive());
            log::info!("shared header: {:?}", link::header());
            log::info!("rendering enabled: {}", renderer::is_enabled());
            renderer::request_reset();
            log::info!("the renderer will be rebuilt on the next frame");
        }
        Action::RestartBlish => {
            let spawned = thread::Builder::new()
                .name("blishhud-restart".into())
                .spawn(|| launcher::restart(config()));
            if let Err(error) = spawned {
                log::error!("could not restart Blish HUD: {error}");
            }
        }
        Action::ToggleRendering => {
            let on = renderer::toggle();
            log::info!("rendering {}", if on { "on" } else { "off" });
        }
    }
}
