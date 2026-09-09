//! # WebAssembly Initiator

mod manifest;
mod source;

use std::collections::BTreeSet;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
pub use manifest::{
    GuestEntry, GuestRoutes, Manifest, Mount, SourceSpec, Transport, TransportKind,
};
use omnia_core::wasmtime::component::Linker;
use omnia_core::wasmtime::{Config, Engine};
use omnia_core::wasmtime_wasi::WasiView;
use omnia_core::{
    ChainPolicy, GuestId, Host, LinkSeam, LoadedGuest, Location, LogMode, MountRegistry, NoLinks,
    Registry, Routes, Runtime, RuntimeOptions, RuntimeParts, Server, StoreCtx, Telemetry, WrpcView,
};
use omnia_link::{FirstArgSelector, GuestSelector, InProcessLinks};
use source::ArtifactPolicy;

use crate::Mode;

/// Builds a [`Deployment`] from an optional programmatic [`Manifest`].
///
/// When no manifest is set, [`build`](Self::build) loads the path in
/// `OMNIA_CONFIG`.
///
/// The safe [`build`](Self::build) rejects pre-compiled (native) artifacts;
/// [`build_trusted`](Self::build_trusted) admits them and is `unsafe` because
/// a pre-compiled artifact is native code the caller must trust.
///
/// ```ignore
/// let deployment = DeploymentBuilder::new()
///     .manifest(Manifest::from_wasm(wasm))
///     .args(args)
///     .mode(mode)
///     .build::<StoreCtx>()
///     .await?;
/// ```
#[derive(Debug, Default)]
pub struct DeploymentBuilder {
    manifest: Option<Manifest>,
    args: Vec<String>,
    mode: Mode,
    allow_empty: bool,
    program_name: Option<String>,
    log_mode: Option<LogMode>,
    guest_timeout: Option<Duration>,
    max_dispatch_depth: Option<usize>,
}

impl DeploymentBuilder {
    /// Start a new builder with no source selected.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the deployment manifest.
    #[must_use]
    pub fn manifest(mut self, manifest: impl Into<Option<Manifest>>) -> Self {
        self.manifest = manifest.into();
        self
    }

    /// Set CLI arguments forwarded to the guest (everything after `--`).
    #[must_use]
    pub fn args(mut self, args: impl Into<Vec<String>>) -> Self {
        self.args = args.into();
        self
    }

    /// Set the deployment drive mode.
    #[must_use]
    pub const fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Mark the deployment as dynamically populated: the guest set may start
    /// empty and grow at run time via
    /// [`Runtime::register`](omnia_core::Runtime::register).
    ///
    /// This only relaxes the "at least one guest" check — static trigger
    /// routing (HTTP/messaging/websocket/CLI) is built at boot; registered
    /// guests are reachable via host-mediated link dispatch and host→guest
    /// [`Dispatcher::invoke`](omnia_core::Dispatcher::invoke).
    #[must_use]
    pub const fn dynamic(mut self) -> Self {
        self.allow_empty = true;
        self
    }

    /// Override the deployment name used for telemetry and — in command mode
    /// — prepended to guest argv as `argv[0]`.
    ///
    /// Defaults to the manifest name (the first `[[guest]]` id, or `omnia`
    /// for an empty dynamic manifest).
    #[must_use]
    pub fn program_name(mut self, name: impl Into<String>) -> Self {
        self.program_name = Some(name.into());
        self
    }

    /// Select the host [`LogMode`] preset installed with telemetry (the
    /// generated direct-command entry peels `--debug` / `--quiet` into this).
    /// Unset defers to `RUST_LOG` alone.
    #[must_use]
    pub const fn log_mode(mut self, mode: LogMode) -> Self {
        self.log_mode = Some(mode);
        self
    }

    /// Override the wall-clock cap on server and server-rooted link-dispatch
    /// invocations for this deployment. Unset defers to `GUEST_TIMEOUT_MS`.
    #[must_use]
    pub const fn guest_timeout(mut self, timeout: Duration) -> Self {
        self.guest_timeout = Some(timeout);
        self
    }

    /// Override the per-chain host-mediated dispatch depth bound for this
    /// deployment. Unset defers to `MAX_DISPATCH_DEPTH`.
    #[must_use]
    pub const fn max_dispatch_depth(mut self, depth: usize) -> Self {
        self.max_dispatch_depth = Some(depth);
        self
    }

    /// Resolve the manifest and build the deployment under `policy`.
    async fn build_inner<T: WasiView + 'static>(
        self, policy: ArtifactPolicy,
    ) -> Result<Deployment<T>> {
        let manifest = if let Some(manifest) = self.manifest {
            manifest
        } else if self.allow_empty {
            // A dynamic deployment may start empty and register guests later.
            Manifest::new()
        } else {
            let config = env::var_os("OMNIA_CONFIG")
                .context("no deployment manifest supplied and OMNIA_CONFIG is unset")?;
            Manifest::from_config(config)?
        };
        manifest.validate(self.allow_empty)?;

        let program_name = self.program_name.unwrap_or_else(|| manifest.name().to_owned());
        // The runtime-carried name read by telemetry, trigger servers, and
        // the bootstrap log. An operator `COMPONENT` override wins over the
        // program name — read once here, never written back to the process
        // environment.
        let name = env::var("COMPONENT").unwrap_or_else(|_| program_name.clone());

        init_telemetry(&name, self.log_mode)?;
        tracing::debug!("initializing runtime");

        let (engine, linker, mut options) = engine_and_linker()?;
        if let Some(timeout) = self.guest_timeout {
            options.guest_timeout = timeout;
        }
        if let Some(depth) = self.max_dispatch_depth {
            options.max_dispatch_depth = depth;
        }

        // Open + identity-stamp every preopen once, here, so a misconfigured
        // mount fails fast at startup rather than per store.
        let mounts = Arc::new(MountRegistry::open(manifest.preopens())?);

        // Guests load (and compile) in parallel through the async
        // [`Source::load`] seam; order still follows the manifest.
        let sources = manifest.sources()?;
        let guests = futures::future::try_join_all(
            sources.iter().map(|source| source.load(&engine, policy)),
        )
        .await?;

        // In command mode the program name is prepended as `argv[0]`.
        let args = if self.mode.is_command() {
            std::iter::once(program_name).chain(self.args).collect()
        } else {
            self.args
        };

        Ok(Deployment {
            name,
            engine,
            linker,
            options,
            guests,
            routes: manifest.routes(),
            links: manifest.link_interfaces(),
            selector: Arc::new(FirstArgSelector),
            mounts,
            args: Arc::new(args),
            mode: self.mode,
            allow_empty: self.allow_empty,
            command_guest: manifest.command_guest(),
            locations: manifest.locations,
        })
    }

    /// Resolve the manifest into a [`Deployment`].
    ///
    /// If no manifest was supplied, the path in `OMNIA_CONFIG` is loaded.
    /// Every guest must be raw component wasm; a pre-compiled (native)
    /// artifact is rejected — see [`build_trusted`](Self::build_trusted).
    ///
    /// # Errors
    ///
    /// Returns an error if no manifest resolves, the manifest is invalid, a
    /// guest names a pre-compiled artifact, or the deployment cannot be built.
    pub async fn build<T: WasiView + 'static>(self) -> Result<Deployment<T>> {
        self.build_inner(ArtifactPolicy::Reject).await
    }

    /// Resolve the manifest into a [`Deployment`], admitting pre-compiled
    /// artifacts.
    ///
    /// If no manifest was supplied, the path in `OMNIA_CONFIG` is loaded.
    ///
    /// # Safety
    ///
    /// Every pre-compiled path this builder's manifest names must identify
    /// trusted, immutable wasmtime output (`omnia compile` /
    /// [`wasmtime::component::Component::serialize`]). A pre-compiled
    /// artifact is native code: wasmtime's compatibility check is not an
    /// authenticity check, and tampered bytes can execute arbitrary code
    /// with host privileges.
    ///
    /// # Errors
    ///
    /// Returns an error if no manifest resolves, the manifest is invalid, or
    /// the deployment cannot be built.
    pub async unsafe fn build_trusted<T: WasiView + 'static>(self) -> Result<Deployment<T>> {
        self.build_inner(ArtifactPolicy::Trust).await
    }
}

/// A compiled set of WebAssembly components with their shared Linker, ready to
/// be [`host`]ed against WASI interfaces and assembled into a [`Registry`].
///
/// [`host`]: Self::host
pub struct Deployment<T: WasiView + 'static> {
    // Deployment name carried onto the runtime for trigger servers and the
    // bootstrap log (the program name, unless `build_inner` honored an
    // operator `COMPONENT` override).
    name: String,
    engine: Engine,
    linker: Linker<T>,
    options: RuntimeOptions,
    guests: Vec<LoadedGuest>,
    routes: Routes,
    // Guest links — the host-mediated interfaces.
    links: BTreeSet<Box<str>>,
    // Host-mediated dispatch selector.
    selector: Arc<dyn GuestSelector>,
    // Mount registry opened from the manifest's resolved preopens.
    mounts: Arc<MountRegistry>,
    // Guest argv threaded into every store. Empty for long-lived servers; in
    // command mode the deployment name is prepended as `argv[0]`.
    args: Arc<Vec<String>>,
    // Whether this deployment runs a one-shot `wasi:cli` command.
    mode: Mode,
    // Whether the guest set may start empty and grow at run time.
    allow_empty: bool,
    // Command-mode guest identity derived from the manifest's marked entry.
    command_guest: Option<GuestId>,
    // The manifest's plugin acquisition locations, carried onto the runtime
    // for the loader capability to install against.
    locations: Vec<Location>,
}

impl<T: WasiView> Deployment<T> {
    /// Link a WASI host's interfaces into the shared Linker.
    ///
    /// # Errors
    ///
    /// Will fail if the host cannot be added to the Linker.
    pub fn host<H, B>(&mut self) -> Result<&mut Self>
    where
        H: Host<T> + Server<B>,
    {
        H::add_to_linker(&mut self.linker)?;
        Ok(self)
    }

    /// Override the host-mediated dispatch [`GuestSelector`].
    ///
    /// Defaults to [`FirstArgSelector`] — the runtime core's "first call argument is the
    /// identity" strategy. Chainable.
    pub fn selector(&mut self, selector: impl GuestSelector) -> &mut Self {
        self.selector = Arc::new(selector);
        self
    }

    /// The deployment name carried onto the runtime for trigger servers and
    /// the bootstrap log.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mount registry built from the deployment's preopens.
    #[must_use]
    pub fn mounts(&self) -> Arc<MountRegistry> {
        Arc::clone(&self.mounts)
    }

    /// Deployment drive mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Borrow the guest argv.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The manifest's plugin acquisition locations.
    #[must_use]
    pub fn plugin_locations(&self) -> &[Location] {
        &self.locations
    }

    /// Assemble the guest [`Registry`].
    ///
    /// Consumes the deployment: pre-instantiation happens once, here, after all
    /// hosts are linked — so no host can be linked after the guests are frozen.
    /// Per call only a fresh instantiate on a new store remains.
    ///
    /// # Errors
    ///
    /// Returns an error if host-mediated imports cannot be polyfilled, a
    /// component cannot be pre-instantiated, or the registry cannot be assembled.
    pub fn into_registry(self) -> Result<Registry<T>>
    where
        // `InProcessLinks` serves over wRPC, which needs the store's wRPC view.
        T: WrpcView,
    {
        let seam: Arc<dyn LinkSeam<T>> = if self.links.is_empty() {
            Arc::new(NoLinks)
        } else {
            Arc::new(InProcessLinks::new(
                self.selector,
                self.links,
                ChainPolicy::from(&self.options),
            ))
        };

        Registry::assemble(
            self.engine,
            self.linker,
            self.options,
            self.guests,
            self.routes,
            seam,
            self.allow_empty,
        )
    }
}

impl<B: Clone + Send + Sync + 'static> Deployment<StoreCtx<B>> {
    /// Assemble this deployment into a [`Runtime`]: registry, handle, then link serve wiring.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be assembled or the link serve
    /// side cannot be wired.
    pub async fn assemble(self, backends: B) -> Result<Runtime<B>> {
        let runtime = Runtime::from_parts(RuntimeParts {
            name: Arc::from(self.name.as_str()),
            args: self.args.to_vec(),
            mounts: Arc::clone(&self.mounts),
            locations: self.locations.clone(),
            command_guest: self.command_guest.clone(),
            backends,
            registry: Arc::new(self.into_registry().context("assembling registry")?),
        });
        runtime.serve_links().await.context("wiring host-mediated link serve side")?;
        Ok(runtime)
    }
}

// Build the shared engine, WASI-linked linker, and runtime options.
fn engine_and_linker<T: WasiView + 'static>() -> Result<(Engine, Linker<T>, RuntimeOptions)> {
    let options = RuntimeOptions::load_env()?;
    let engine = Engine::new(&Config::from(&options))?;

    // register services with runtime's Linker
    let mut linker = Linker::new(&engine);
    omnia_core::wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    omnia_core::wasmtime_wasi::p3::add_to_linker(&mut linker)?;

    Ok((engine, linker, options))
}

// Initialize telemetry for the runtime.
//
// Telemetry initialization is idempotent (`Telemetry::build`): the first call
// in the process — here or in an embedder — installs the subscriber and
// providers, and later deployments reuse them.
fn init_telemetry(name: &str, log_mode: Option<LogMode>) -> Result<()> {
    let mut builder = Telemetry::new(name);
    if let Ok(endpoint) = env::var("OTEL_GRPC_URL") {
        builder = builder.endpoint(endpoint);
    } else {
        tracing::debug!("OTEL_GRPC_URL unset; using OpenTelemetry defaults");
    }
    if let Some(mode) = log_mode {
        builder = builder.log_mode(mode);
    }
    builder.build().context("initializing telemetry")
}

#[cfg(test)]
mod tests {
    use omnia_core::RuntimeOptions;
    use omnia_core::wasmtime::{Config, Engine};

    #[test]
    fn builds_pooling() {
        // Independent totals plus per-component/per-module limits, sized small
        // (and with a tiny per-memory cap) so the reservation stays cheap.
        let options = RuntimeOptions {
            pool_max_instances: 8,
            pool_total_core_instances: 8,
            pool_total_memories: 16,
            pool_total_tables: 16,
            pool_total_stacks: 8,
            pool_max_memory_bytes: Some(1 << 20),
            pool_max_memories_per_component: Some(4),
            pool_max_tables_per_component: Some(4),
            pool_max_memories_per_module: Some(2),
            pool_max_tables_per_module: Some(2),
            pool_decommit_batch_size: 8,
            ..RuntimeOptions::load_env().expect("should load")
        };
        Engine::new(&Config::from(&options))
            .expect("decoupled multi-memory pooling config should build an engine");
    }

    #[test]
    fn builds_no_pooling() {
        let options = RuntimeOptions {
            pooling: false,
            ..RuntimeOptions::load_env().expect("should load")
        };
        Engine::new(&Config::from(&options)).expect("non-pooling config should build an engine");
    }
}
