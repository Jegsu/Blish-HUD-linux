//! The crate's error type.

/// Everything that can go wrong in the bridge.
///
/// Nothing in the bridge is allowed to take the game down, so failures are always surfaced as
/// values and logged by whoever gives up on the operation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Windows API call failed.
    #[cfg(windows)]
    #[error(transparent)]
    Windows(#[from] windows::core::Error),

    /// A function hook could not be installed or removed.
    #[cfg(windows)]
    #[error("hook: {0}")]
    Hook(#[from] retour::Error),

    /// A file could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A key combination could not be understood.
    #[error("invalid key combination `{0}`")]
    InvalidKeyCombo(String),

    /// A keybind named an action that does not exist.
    #[error("unknown action `{0}`")]
    UnknownAction(String),

    /// The config file named a setting that does not exist.
    #[error("unknown setting `{0}`")]
    UnknownSetting(String),

    /// A setting had a value of the wrong kind.
    #[error("invalid value `{value}` for `{key}`")]
    InvalidValue {
        /// The setting.
        key: String,
        /// What it was set to.
        value: String,
    },

    /// Something the bridge depends on is missing or unusable.
    #[error("{0}")]
    Unavailable(String),
}

/// A `Result` defaulting to the crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
