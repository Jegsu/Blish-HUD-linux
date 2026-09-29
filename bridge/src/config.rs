//! The `bridge.ini` configuration file.
//!
//! The format is deliberately small: `key = value` lines, `#` or `;` comments, a `[general]`
//! section for settings, and a `[keybinds]` section mapping key combinations to actions.
//! Settings above any section header count as `[general]`. Parsing is lenient — a bad line is
//! reported and skipped rather than failing the whole file, because a typo in the config must
//! never cost the user their overlay.

use std::path::PathBuf;

use crate::{
    Error,
    keybind::{Action, KeyCombo, Keybind},
};

/// Name of the config file, inside the bridge's data directory.
pub const FILE_NAME: &str = "bridge.ini";

/// The config written on first run. [`Config::default`] must match it exactly; a test enforces
/// that.
pub const TEMPLATE: &str = "\
[general]
launch_blish = true
blish_path = addons/blishhud/Blish HUD.exe
chainload =

[keybinds]
Ctrl+Alt+P = dump_state
Ctrl+Alt+O = restart_blish
Ctrl+Alt+B = toggle_rendering
";

/// Bridge settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Whether to start Blish HUD when the game starts.
    pub launch_blish: bool,
    /// Blish HUD's executable.
    pub blish_path: PathBuf,
    /// A d3d11 proxy dll to load and forward the game's D3D11 calls through, such as arcdps.
    pub chainload: Option<PathBuf>,
    /// Active keybinds.
    pub keybinds: Vec<Keybind>,
}

impl Default for Config {
    fn default() -> Self {
        let bind = |key: u8, action| Keybind {
            combo: KeyCombo {
                key: u32::from(key),
                ctrl: true,
                alt: true,
                shift: false,
            },
            action,
        };

        Self {
            launch_blish: true,
            blish_path: PathBuf::from("addons/blishhud/Blish HUD.exe"),
            chainload: None,
            keybinds: vec![
                bind(b'P', Action::DumpState),
                bind(b'O', Action::RestartBlish),
                bind(b'B', Action::ToggleRendering),
            ],
        }
    }
}

/// The outcome of parsing a config file.
#[derive(Debug)]
pub struct Parsed {
    /// The settings, with defaults for anything missing or unreadable.
    pub config: Config,
    /// One message per line that could not be used.
    pub warnings: Vec<String>,
}

#[derive(PartialEq, Eq)]
enum Section {
    General,
    Keybinds,
    Unknown,
}

/// Parses a config file.
///
/// A `[keybinds]` section replaces the default keybinds entirely, so binds can be removed by
/// leaving them out. Without one, the defaults apply.
pub fn parse(text: &str) -> Parsed {
    let mut config = Config::default();
    let mut warnings = Vec::new();
    let mut keybinds: Option<Vec<Keybind>> = None;
    let mut section = Section::General;

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }

        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = match name.trim().to_ascii_lowercase().as_str() {
                "general" => Section::General,
                "keybinds" => {
                    keybinds.get_or_insert_with(Vec::new);
                    Section::Keybinds
                }
                other => {
                    warnings.push(format!("line {line_number}: unknown section [{other}]"));
                    Section::Unknown
                }
            };
            continue;
        }

        let Some((key, value)) = line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) else {
            warnings.push(format!("line {line_number}: expected `key = value`"));
            continue;
        };

        let result = match section {
            Section::General => apply_setting(&mut config, key, value),
            Section::Keybinds => parse_keybind(key, value)
                .map(|bind| keybinds.get_or_insert_with(Vec::new).push(bind)),
            Section::Unknown => Ok(()),
        };
        if let Err(error) = result {
            warnings.push(format!("line {line_number}: {error}"));
        }
    }

    if let Some(keybinds) = keybinds {
        config.keybinds = keybinds;
    }

    Parsed { config, warnings }
}

fn apply_setting(config: &mut Config, key: &str, value: &str) -> Result<(), Error> {
    match key.to_ascii_lowercase().as_str() {
        "launch_blish" => config.launch_blish = parse_bool(key, value)?,
        "blish_path" => config.blish_path = PathBuf::from(value),
        "chainload" => config.chainload = (!value.is_empty()).then(|| PathBuf::from(value)),
        _ => return Err(Error::UnknownSetting(key.to_owned())),
    }
    Ok(())
}

fn parse_keybind(combo: &str, action: &str) -> Result<Keybind, Error> {
    Ok(Keybind {
        combo: combo.parse()?,
        action: action.parse()?,
    })
}

fn parse_bool(key: &str, value: &str) -> Result<bool, Error> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(Error::InvalidValue {
            key: key.to_owned(),
            value: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_clean(text: &str) -> Config {
        let parsed = parse(text);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        parsed.config
    }

    #[test]
    fn template_matches_the_defaults() {
        assert_eq!(parse_clean(TEMPLATE), Config::default());
    }

    #[test]
    fn empty_file_uses_the_defaults() {
        assert_eq!(parse_clean(""), Config::default());
    }

    #[test]
    fn reads_general_settings() {
        let config = parse_clean(
            "launch_blish = no\n\
             blish_path = C:/Blish/Blish HUD.exe\n\
             chainload = addons/arcdps/d3d11.dll\n",
        );

        assert!(!config.launch_blish);
        assert_eq!(config.blish_path, PathBuf::from("C:/Blish/Blish HUD.exe"));
        assert_eq!(
            config.chainload,
            Some(PathBuf::from("addons/arcdps/d3d11.dll"))
        );
    }

    #[test]
    fn reads_the_general_section() {
        let config = parse_clean("[general]\nlaunch_blish = no\n[keybinds]\n");

        assert!(!config.launch_blish);
    }

    #[test]
    fn empty_chainload_disables_it() {
        assert_eq!(parse_clean("chainload =").chainload, None);
    }

    #[test]
    fn keybinds_section_replaces_the_defaults() {
        let config = parse_clean("[keybinds]\nShift+F5 = toggle_rendering\n");

        assert_eq!(
            config.keybinds,
            vec![Keybind {
                combo: "Shift+F5".parse().unwrap(),
                action: Action::ToggleRendering,
            }]
        );
    }

    #[test]
    fn empty_keybinds_section_removes_all_binds() {
        assert!(parse_clean("[keybinds]\n").keybinds.is_empty());
    }

    #[test]
    fn bad_lines_are_skipped_with_a_warning() {
        let parsed = parse(
            "launch_blish = maybe\n\
             not a setting\n\
             colour = blue\n\
             [keybinds]\n\
             Ctrl+Q = self_destruct\n\
             Ctrl+Alt+B = toggle_rendering\n",
        );

        assert_eq!(parsed.warnings.len(), 4, "{:?}", parsed.warnings);
        assert!(parsed.warnings[0].starts_with("line 1:"));
        // Good lines around the bad ones still apply.
        assert!(parsed.config.launch_blish);
        assert_eq!(parsed.config.keybinds.len(), 1);
    }

    #[test]
    fn unknown_sections_are_ignored_with_a_warning() {
        let parsed = parse("[future]\nsomething = 1\n");

        assert_eq!(parsed.warnings.len(), 1);
        assert_eq!(parsed.config, Config::default());
    }

    #[test]
    fn keys_and_sections_are_case_insensitive() {
        let config =
            parse_clean("LAUNCH_BLISH = FALSE\n[KeyBinds]\nctrl+alt+b = TOGGLE_RENDERING\n");

        assert!(!config.launch_blish);
        assert_eq!(config.keybinds.len(), 1);
    }
}
