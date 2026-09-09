#![doc = include_str!("../README.md")]
#![cfg(not(target_arch = "wasm32"))]
#![allow(unsafe_code)] // `DeploymentBuilder::build_trusted` and `Source::load`

// The embedder facade: the runtime spine (`omnia-core`), the plugins
// capability (`omnia-plugin`, behind the `plugin` feature), the `run` grammar
// (`omnia-cli`, behind the `cli` feature), and the `runtime!` macro,
// re-exported under one root. The `runtime!` macro emits `omnia::…` paths, so
// every name it references must stay reachable from here — the plugin names
// only when the invocation declares `locations:`, which is what the feature
// gates.
//
// `#[doc(inline)]` matters: rustdoc renders a cross-crate `pub use` as a bare
// re-export line pointing into the source crate, so without it every item
// page (and every path readers copy) would spell `omnia_core::…`. Inlining
// keeps the documented surface at `omnia::…`, the only path embedders use.

// `anyhow` is the error vocabulary of `Backend`, `Wiring`, and the generated
// runtime module; `futures` supplies the `BoxFuture` in the plugin store and
// acquirer seams (`ContentStore`, `ReleaseStore`, `PathSource`,
// `RegistrySource`) and the generated `serve` hook. Both are part of the
// facade's public signatures, so embedders reach them from here without a
// direct dependency of their own; `futures` stays unconditional because the
// macro output uses it whether or not the plugin surface is enabled.
#[cfg(feature = "jit")]
pub mod compile;
mod deployment;
mod entry;
mod lifecycle;
pub use anyhow;
pub use futures;
#[cfg(feature = "cli")]
#[doc(inline)]
pub use omnia_cli::{Cli, Command, Parser};
#[doc(inline)]
pub use omnia_core::{
    AdmitError, Backend, ChainPolicy, CliRoutes, Dispatcher, ExitStatus, Extensions, FromEnv,
    FutureResult, Guest, GuestArtifact, GuestId, HasDispatcher, HasExtensions, HasLimits,
    HasMounts, HasTable, Host, HostCtx, HttpBorrow, HttpCtx, HttpRoutes, LinkClient, LinkSeam,
    Location, LogMode, MountRegistry, NoLinks, NoOptions, PatternRoutes, Provides, Proxy, Registry,
    ResolvedPreopen, Routes, Runtime, RuntimeOptions, RuntimeParts, Server, StoreBase, StoreConfig,
    StoreCtx, StoreFactory, StoreView, Telemetry, TriggerRouter, WeakRuntime, WrpcState,
    as_command_chain, get_cloned, host_error, serve_links, sha256_digest, telemetry, wasi_view,
};
#[doc(hidden)]
pub use omnia_core::{WrpcCtxView, WrpcView, pastey, tokio, wasmtime, wasmtime_wasi};
#[doc(inline)]
pub use omnia_host_macros::runtime;
#[doc(inline)]
pub use omnia_link::{FirstArgSelector, GuestSelector, InProcessLinks};
#[cfg(feature = "plugin")]
#[doc(inline)]
pub use omnia_plugin::{
    ContentStore, LoadError, NoStore, Origin, PathMounts, PathSource, Plugin, PluginLoader,
    Plugins, RegistryClient, RegistrySource, ReleaseStore, WasiPlugins, WasiPluginsCtxView,
};

pub use self::deployment::{
    Deployment, DeploymentBuilder, GuestEntry, GuestRoutes, Manifest, Mount, SourceSpec, Transport,
    TransportKind,
};
#[doc(hidden)]
pub use self::entry::{MainOptions, ManifestSource, main};
pub use self::lifecycle::{Backends, Mode, Wiring};
#[doc(hidden)]
pub use self::lifecycle::{run, run_with};
