//! Log file and panic reporting.

use std::{
    fs::{self, OpenOptions},
    path::Path,
    sync::Once,
    time::{Duration, SystemTime},
};

use chrono::Local;

/// Logs older than this are deleted at startup.
const KEEP_LOGS_FOR: Duration = Duration::from_secs(24 * 60 * 60);

static INIT: Once = Once::new();

/// Starts logging to a timestamped file under `dir/logs`, and to stdout, which Wine forwards to
/// the terminal the game was started from. Only the first call does anything.
///
/// Never fails: without a writable directory, logging continues on stdout alone.
pub fn init(dir: &Path) {
    INIT.call_once(|| {
        let logs = dir.join("logs");
        let file = fs::create_dir_all(&logs).ok().and_then(|()| {
            remove_old_logs(&logs);
            let name = format!("bridge-{}.log", Local::now().format("%Y-%m-%d_%H-%M-%S"));
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(logs.join(name))
                .ok()
        });

        let mut dispatch = fern::Dispatch::new()
            .level(log::LevelFilter::Info)
            .format(|out, message, record| {
                out.finish(format_args!(
                    "[{}] [{}] [{}] {}",
                    Local::now().format("%Y-%m-%d %H:%M:%S"),
                    record.level(),
                    record.target(),
                    message
                ));
            })
            .chain(std::io::stdout());
        if let Some(file) = file {
            dispatch = dispatch.chain(file);
        }
        // Fails only if a logger is already installed, which leaves logging working anyway.
        let _ = dispatch.apply();

        std::panic::set_hook(Box::new(|info| {
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic>");
            let location = info
                .location()
                .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
            log::error!("panic at {location}: {payload}");
        }));
    });
}

fn remove_old_logs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();

    for entry in entries.flatten() {
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age > KEEP_LOGS_FOR)
            });
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }
}
