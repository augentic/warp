//! The loader's refusal vocabulary, owned here so every module below the WIT
//! adapter depends downward on plain Rust rather than upward on bindgen
//! output.

use std::fmt;

use omnia_core::AdmitError;

/// Why a load was refused — the host mirror of the `omnia:plugins/loader`
/// `error` variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The request or deployment is wrong and a retry cannot succeed.
    Refused(String),
    /// The acquirer could not produce the package bytes; a retry may succeed.
    Unavailable(String),
    /// The package identity is already registered and cannot be re-bound.
    AlreadyActive(String),
    /// Loader misconfiguration or an internal registration failure.
    Internal(String),
}

impl LoadError {
    // The shared refusal for a deployment that linked the loader host but
    // installed no `Plugins` extension: either the macro's `locations:` list
    // was never declared, or a bare `plugins: {}` beside `config:` pointed at
    // a TOML with no `[[location]]` entries.
    pub(crate) fn no_plugins(package: &str) -> Self {
        Self::Internal(format!(
            "this deployment has no plugins; declare a location (`plugins: {{ locations: [...] \
             }}` inline, or `[[location]]` in the config file) to load `{package}`"
        ))
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(detail) => write!(f, "refused: {detail}"),
            Self::Unavailable(detail) => write!(f, "unavailable: {detail}"),
            Self::AlreadyActive(detail) => write!(f, "already-active: {detail}"),
            Self::Internal(detail) => write!(f, "internal: {detail}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<AdmitError> for LoadError {
    fn from(error: AdmitError) -> Self {
        match error {
            AdmitError::ArtifactRefused(reason) => Self::Refused(reason),
            AdmitError::AlreadyRegistered(reason) => Self::AlreadyActive(reason),
            AdmitError::Internal(reason) => Self::Internal(reason),
        }
    }
}
