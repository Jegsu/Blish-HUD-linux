//! Key combinations and the actions they trigger.

use std::{fmt, str::FromStr};

use crate::Error;

/// Something a keybind can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Log the bridge's state and rebuild the renderer.
    DumpState,
    /// Restart the Blish HUD process started by the bridge.
    RestartBlish,
    /// Stop or resume drawing Blish HUD.
    ToggleRendering,
}

impl Action {
    /// Every action, for documentation and exhaustive tests.
    pub const ALL: [Self; 3] = [Self::DumpState, Self::RestartBlish, Self::ToggleRendering];

    /// The name used for this action in the config file.
    pub const fn name(self) -> &'static str {
        match self {
            Self::DumpState => "dump_state",
            Self::RestartBlish => "restart_blish",
            Self::ToggleRendering => "toggle_rendering",
        }
    }
}

impl FromStr for Action {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|action| action.name().eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| Error::UnknownAction(name.trim().to_owned()))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A key plus the modifiers that must be held with it, such as `Ctrl+Alt+P`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    /// The Win32 virtual key code.
    pub key: u32,
    /// Whether Ctrl must be held.
    pub ctrl: bool,
    /// Whether Alt must be held.
    pub alt: bool,
    /// Whether Shift must be held.
    pub shift: bool,
}

/// Virtual key code of F1; F2..F24 follow consecutively.
const VK_F1: u32 = 0x70;

impl KeyCombo {
    /// Whether a key press with the given modifier state triggers this combination. Modifiers
    /// must match exactly, so `Ctrl+P` does not fire on `Ctrl+Shift+P`.
    pub fn matches(&self, key: u32, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.key == key && self.ctrl == ctrl && self.alt == alt && self.shift == shift
    }

    /// Parses a key name into its virtual key code: `A`-`Z`, `0`-`9`, or `F1`-`F24`.
    fn parse_key(name: &str) -> Option<u32> {
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            // Letters and digits map to their uppercase ASCII codes.
            return c
                .is_ascii_alphanumeric()
                .then(|| u32::from(c.to_ascii_uppercase()));
        }

        let number = name
            .strip_prefix(['F', 'f'])?
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=24).contains(n))?;
        Some(VK_F1 + number - 1)
    }
}

impl FromStr for KeyCombo {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || Error::InvalidKeyCombo(text.trim().to_owned());

        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let key = parts.pop().and_then(Self::parse_key).ok_or_else(invalid)?;

        let mut combo = Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        };

        for modifier in parts {
            let held = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut combo.ctrl,
                "alt" => &mut combo.alt,
                "shift" => &mut combo.shift,
                _ => return Err(invalid()),
            };
            // A repeated modifier is almost certainly a typo for a different one.
            if *held {
                return Err(invalid());
            }
            *held = true;
        }

        Ok(combo)
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if held {
                f.write_str(name)?;
            }
        }
        match self.key {
            key @ VK_F1..=0x87 => write!(f, "F{}", key - VK_F1 + 1),
            key => match char::from_u32(key) {
                Some(c) => write!(f, "{c}"),
                None => write!(f, "{key:#04x}"),
            },
        }
    }
}

/// A key combination bound to an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keybind {
    /// What has to be pressed.
    pub combo: KeyCombo,
    /// What it does.
    pub action: Action,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> KeyCombo {
        text.parse().unwrap()
    }

    #[test]
    fn parses_modifiers_and_key() {
        assert_eq!(
            combo("Ctrl+Alt+P"),
            KeyCombo {
                key: u32::from(b'P'),
                ctrl: true,
                alt: true,
                shift: false,
            }
        );
    }

    #[test]
    fn letters_are_case_insensitive() {
        // A lowercase key must not turn into its ASCII code: `p` is 0x70, which is VK_F1.
        assert_eq!(combo("ctrl+alt+p"), combo("Ctrl+Alt+P"));
    }

    #[test]
    fn tolerates_whitespace() {
        assert_eq!(combo(" Ctrl + Shift + 5 "), combo("Ctrl+Shift+5"));
    }

    #[test]
    fn parses_function_keys() {
        assert_eq!(combo("F1").key, 0x70);
        assert_eq!(combo("Alt+f12").key, 0x7B);
        assert_eq!(combo("F24").key, 0x87);
    }

    #[test]
    fn rejects_malformed_combinations() {
        for text in [
            "",
            "Ctrl+",
            "Ctrl+Alt",
            "Hyper+P",
            "Ctrl+Ctrl+P",
            "Ctrl+PP",
            "F0",
            "F25",
            "Ctrl+!",
        ] {
            assert!(text.parse::<KeyCombo>().is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let bind = combo("Ctrl+P");
        let p = u32::from(b'P');

        assert!(bind.matches(p, true, false, false));
        assert!(!bind.matches(p, true, false, true));
        assert!(!bind.matches(p, false, false, false));
    }

    #[test]
    fn displays_in_config_syntax() {
        for text in ["Ctrl+Alt+P", "Shift+F5", "7", "Ctrl+Alt+Shift+Z"] {
            assert_eq!(combo(text).to_string(), text);
        }
    }

    #[test]
    fn every_action_round_trips_through_its_name() {
        for action in Action::ALL {
            assert_eq!(action.name().parse::<Action>().unwrap(), action);
        }
    }

    #[test]
    fn rejects_unknown_actions() {
        assert!(matches!(
            "launch_rockets".parse::<Action>(),
            Err(Error::UnknownAction(name)) if name == "launch_rockets"
        ));
    }
}
