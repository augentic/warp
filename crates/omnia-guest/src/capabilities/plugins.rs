//! Plugin-loading (requester) capability over `omnia:plugins/loader`.
//!
//! The requester surface for any application that late-binds guests into its
//! deployment's declared plugin seams: the guest names code — a package, a
//! location, and an optional content pin — and the host acquires, verifies,
//! validates, and registers it, handing back a typed [`Plugin`] handle.
//! Component bytes never cross the interface in either direction.

use std::collections::BTreeMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

/// Bindings for the `omnia:plugins` imports world.
#[cfg(target_arch = "wasm32")]
mod generated {
    #![allow(missing_docs)]

    wit_bindgen::generate!({
        world: "imports",
        path: "wit",
        generate_all,
    });
}

/// The canonical digest scheme prefix.
const SCHEME: &str = "sha256:";

/// Hex characters in a sha256 digest.
const HEX_LEN: usize = 64;

/// Where the deployment's acquirer finds a package's component bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// A package registry; `None` selects the acquirer's default.
    Registry(Option<String>),
    /// A location-relative component path, read fresh on every load.
    Path(String),
}

/// A validated `sha256:<hex>` content digest, canonicalized to lowercase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    /// The canonical `sha256:<hex>` digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(pin: &str) -> Result<Self, Error> {
        let Some(hex) = pin.strip_prefix(SCHEME) else {
            return Err(Error::Refused(format!(
                "digest pin `{pin}` does not use the `sha256:<hex>` scheme"
            )));
        };
        if hex.len() != HEX_LEN || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Refused(format!(
                "digest pin `{pin}` is not {HEX_LEN} hex characters"
            )));
        }
        Ok(Self(format!("{SCHEME}{}", hex.to_ascii_lowercase())))
    }
}

impl TryFrom<String> for Digest {
    type Error = Error;

    fn try_from(pin: String) -> Result<Self, Error> {
        pin.parse()
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The (package, location, pin) triple a requester names for one load.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct PluginRef {
    /// The wasm-pkg package identity to register under (also the routed
    /// dispatch identity), for example `emery:intent@1.0.0`.
    #[builder(into)]
    pub package: String,
    /// Where the deployment's acquirer finds the component bytes.
    pub location: Location,
    /// Optional content pin, verified host-side before validation.
    pub digest: Option<Digest>,
}

/// A loaded plugin: the routed dispatch identity plus its resolved digest.
///
/// A plain value — loading confers no lifecycle authority over the loaded
/// component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    id: String,
    digest: Digest,
}

impl Plugin {
    /// A handle over a routed identity and its resolved digest — the
    /// constructor native suites use to script loads.
    #[must_use]
    pub fn new(id: impl Into<String>, digest: Digest) -> Self {
        Self {
            id: id.into(),
            digest,
        }
    }

    /// The routed identity host-mediated dispatch keys on.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The resolved content digest of the loaded bytes — commit it as a pin
    /// to make an unpinned load reproducible.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// Typed load refusal, mirroring the `omnia:plugins/loader` error variant.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The request or deployment is wrong and a retry cannot succeed: an
    /// unserved location kind, a malformed or mismatched digest pin, or an
    /// invalid artifact.
    #[error("refused: {0}")]
    Refused(String),
    /// The acquirer could not produce the package bytes; the source may
    /// recover, so a retry can succeed.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The package identity is already active and cannot be re-bound.
    #[error("already active: {0}")]
    AlreadyActive(String),
    /// Loader misconfiguration or an internal registration failure.
    #[error("internal: {0}")]
    Internal(String),
}

impl Error {
    /// The kebab-case wire discriminant, stable for callers to branch on.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Refused(_) => "refused",
            Self::Unavailable(_) => "unavailable",
            Self::AlreadyActive(_) => "already-active",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        let code = error.code().to_owned();
        match error {
            Error::Unavailable(description) => Self::BadGateway { code, description },
            Error::Internal(description) => Self::ServerError { code, description },
            Error::Refused(description) | Error::AlreadyActive(description) => {
                Self::BadRequest { code, description }
            }
        }
    }
}

/// Plugin loading (Omnia Plugins).
///
/// The default WASM implementation delegates to `omnia:plugins/loader`; off
/// `wasm32` the signature is bare so native suites script loads.
pub trait Plugins: Send + Sync {
    /// Request the host load `plugin`, idempotent on (package, digest); the
    /// returned handle carries the routed identity and resolved digest.
    ///
    /// # Errors
    ///
    /// Returns the loader's typed refusal ([`Error`]) when the host cannot
    /// acquire, verify, validate, or register the component.
    #[cfg(not(target_arch = "wasm32"))]
    fn load(&self, plugin: &PluginRef) -> impl Future<Output = Result<Plugin, Error>> + Send;

    /// Request the host load `plugin`, idempotent on (package, digest); the
    /// returned handle carries the routed identity and resolved digest.
    ///
    /// # Errors
    ///
    /// Returns the loader's typed refusal ([`Error`]) when the host cannot
    /// acquire, verify, validate, or register the component.
    #[cfg(target_arch = "wasm32")]
    fn load(&self, plugin: &PluginRef) -> impl Future<Output = Result<Plugin, Error>> + Send {
        use generated::omnia::plugins::loader;

        let package = plugin.package.clone();
        let from = match &plugin.location {
            Location::Registry(registry) => loader::Location::Registry(registry.clone()),
            Location::Path(path) => loader::Location::Path(path.clone()),
        };
        let pin = plugin.digest.as_ref().map(|digest| digest.as_str().to_owned());
        async move {
            let loaded = loader::load(package, from, pin).await?;
            let digest = loaded.digest.parse().map_err(|error: Error| {
                Error::Internal(format!("host reported a malformed digest: {error}"))
            })?;
            Ok(Plugin::new(loaded.id, digest))
        }
    }
}

delegate_deref!(Plugins {
    fn load(&self, plugin: &PluginRef) -> impl Future<Output = Result<Plugin, Error>> + Send {
        (**self).load(plugin)
    }
});

/// The WASI-backed provider a `wasm32` guest hands its wasm-free core; the
/// default method body carries the whole delegation.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug)]
pub struct WasiPlugins;

#[cfg(target_arch = "wasm32")]
impl Plugins for WasiPlugins {}

/// Wire conversion from the `omnia:plugins/loader` refusal variant.
#[cfg(target_arch = "wasm32")]
mod wire {
    use super::Error;
    use super::generated::omnia::plugins::loader;

    impl From<loader::Error> for Error {
        fn from(error: loader::Error) -> Self {
            match error {
                loader::Error::Refused(detail) => Self::Refused(detail),
                loader::Error::Unavailable(detail) => Self::Unavailable(detail),
                loader::Error::AlreadyActive(detail) => Self::AlreadyActive(detail),
                loader::Error::Internal(detail) => Self::Internal(detail),
            }
        }
    }
}

/// Ensure-once memoization over a [`Plugins`] provider: handles are memoized
/// by package identity for the instance's lifetime — never bytes, whose
/// caching stays inside the host's acquirer.
pub struct PluginCache<P: Plugins> {
    provider: P,
    loaded: Mutex<BTreeMap<String, Plugin>>,
}

impl<P: Plugins> PluginCache<P> {
    /// An empty memo over `provider`.
    #[must_use]
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            loaded: Mutex::new(BTreeMap::new()),
        }
    }

    /// Load `plugin` at most once per package: a memo hit returns the held
    /// handle without touching the host.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyActive`] when a memoized package is re-pinned
    /// to a different digest (mirroring the host's refusal), or the
    /// provider's own [`Error`] on a cold load.
    pub async fn ensure(&self, plugin: &PluginRef) -> Result<Plugin, Error> {
        let held = self.lock().get(&plugin.package).cloned();
        if let Some(held) = held {
            return match &plugin.digest {
                Some(pin) if pin != held.digest() => Err(Error::AlreadyActive(format!(
                    "`{}` is active with digest {}",
                    plugin.package,
                    held.digest()
                ))),
                _ => Ok(held),
            };
        }
        // The lock is never held across the await: a racing duplicate load
        // is harmless because the host load is idempotent.
        let handle = self.provider.load(plugin).await?;
        self.lock().insert(plugin.package.clone(), handle.clone());
        Ok(handle)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Plugin>> {
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A cache is itself a [`Plugins`] provider, so a caller bounded on the
/// capability memoizes without naming the cache: every `load` is an
/// [`ensure`](PluginCache::ensure).
impl<P: Plugins> Plugins for PluginCache<P> {
    fn load(&self, plugin: &PluginRef) -> impl Future<Output = Result<Plugin, Error>> + Send {
        self.ensure(plugin)
    }
}

#[cfg(test)]
mod tests {
    use super::{Digest, Error};

    #[test]
    fn digest_canonicalizes_case() {
        let parsed: Digest =
            format!("sha256:{}", "AB".repeat(32)).parse().expect("uppercase hex accepted");
        assert_eq!(parsed.as_str(), format!("sha256:{}", "ab".repeat(32)));
    }

    #[test]
    fn digest_malformed() {
        for pin in [
            format!("sha512:{}", "ab".repeat(32)),
            "sha256:abcd".into(),
            format!("sha256:{}", "zz".repeat(32)),
        ] {
            let error = pin.parse::<Digest>().expect_err("malformed pin refused");
            assert!(matches!(error, Error::Refused(_)), "{pin} refused");
        }
    }

    #[test]
    fn digest_serde() {
        let json = format!("\"sha256:{}\"", "ab".repeat(32));
        let parsed: Digest = serde_json::from_str(&json).expect("valid digest deserializes");
        assert_eq!(serde_json::to_string(&parsed).expect("serializes"), json);
        serde_json::from_str::<Digest>("\"sha256:abcd\"").expect_err("malformed digest refused");
    }

    #[test]
    fn taxonomy_mapping() {
        let cases = [
            (Error::Refused("r".into()), "refused"),
            (Error::Unavailable("u".into()), "unavailable"),
            (Error::AlreadyActive("c".into()), "already-active"),
            (Error::Internal("x".into()), "internal"),
        ];
        for (error, code) in cases {
            let mapped = crate::Error::from(error.clone());
            assert_eq!(mapped.code(), code);
            match error {
                Error::Unavailable(_) => {
                    assert!(matches!(mapped, crate::Error::BadGateway { .. }));
                }
                Error::Internal(_) => {
                    assert!(matches!(mapped, crate::Error::ServerError { .. }));
                }
                _ => assert!(matches!(mapped, crate::Error::BadRequest { .. })),
            }
        }
    }
}
