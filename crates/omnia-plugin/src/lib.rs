//! # Late-bound plugins
//!
//! The `omnia:plugins/loader` capability crate: a guest names code (package,
//! location, optional sha256 pin) and the host acquires, verifies, and admits
//! it through the runtime's admission seam, handing back a typed [`Plugin`]
//! handle. Component bytes never cross the interface in either direction, and
//! the requester receives no lifecycle authority — validation, compilation,
//! and publication stay host-side, bounded by the deployment's declared
//! plugin interfaces.
//!
//! Everything plugin lives here: the [`WasiPlugins`] host binding, the
//! [`Plugins`] load path, and the acquisition seam. Acquisition policy
//! (endpoints, cache, path reads) is the two slots [`Plugins::install`]
//! takes — one per [`Origin`] kind — from the deployment's `Wiring::extend`
//! hook. [`Plugins::install_declared`] fills them from the deployment's
//! declared locations (the `runtime!` macro's `plugin: { locations: [...] }`
//! list, carried as manifest data). The built-in acquirers are
//! [`PathMounts`] and [`RegistryClient`]; a store behind `RegistryClient`
//! implements [`ContentStore`] and [`ReleaseStore`]. The runtime core keeps
//! zero storage and network dependencies.
//!
//! Embedders — deployments and store implementors alike — reach all of this
//! through the `omnia` facade's re-exports, never by depending on this crate
//! or on `omnia-core` directly; those are dependencies for building another
//! capability crate.

mod admission;
mod declared;
mod error;
mod host;
mod loader;
mod path;
mod registry;
mod source;
mod store;

pub use self::error::LoadError;
pub use self::host::{WasiPlugins, WasiPluginsCtxView};
pub use self::loader::{Plugin, PluginLoader, Plugins};
pub use self::path::PathMounts;
pub use self::registry::RegistryClient;
pub use self::source::{Origin, PathSource, RegistrySource};
pub use self::store::{ContentStore, NoStore, ReleaseStore};
