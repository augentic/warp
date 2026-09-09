//! Connected runtime: [`Runtime`], [`RuntimeParts`], [`WeakRuntime`], and [`ExitStatus`].

mod command;

use std::fmt;
use std::sync::{Arc, Weak};

use anyhow::{Context as _, Result};
use wasmtime::Store;
use wasmtime::component::{Component, Instance, InstancePre};

use crate::artifact::GuestArtifact;
use crate::extensions::Extensions;
use crate::location::Location;
use crate::mount::MountRegistry;
use crate::registry::{Guest, GuestId, HttpRoutes, PublishError, TriggerRouter};
use crate::store::HasLimits;
use crate::{Dispatcher, Registry, RuntimeOptions, StoreBase, StoreCtx};

/// Guest exit code. [`code_u8`](Self::code_u8) and [`ExitCode`](std::process::ExitCode)
/// keep only the low byte (POSIX semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus(i32);

impl ExitStatus {
    /// Exit code `0`.
    pub const SUCCESS: Self = Self(0);

    /// Full `i32` exit code from the guest.
    #[must_use]
    pub const fn code(self) -> i32 {
        self.0
    }

    /// Low byte of the exit code (POSIX process status).
    #[must_use]
    pub const fn code_u8(self) -> u8 {
        self.0.to_le_bytes()[0]
    }
}

impl From<i32> for ExitStatus {
    fn from(code: i32) -> Self {
        Self(code)
    }
}

impl From<ExitStatus> for std::process::ExitCode {
    fn from(status: ExitStatus) -> Self {
        Self::from(status.code_u8())
    }
}

/// Inputs to [`Runtime::from_parts`].
pub struct RuntimeParts<B: 'static> {
    /// Deployment name read by trigger servers and the bootstrap log.
    pub name: Arc<str>,
    /// Assembled guest registry.
    pub registry: Arc<Registry<StoreCtx<B>>>,
    /// Guest argv.
    pub args: Vec<String>,
    /// Mount registry opened from the deployment's preopens.
    pub mounts: Arc<MountRegistry>,
    /// Connected backend bundle.
    pub backends: B,
    /// Plugin acquisition locations from the manifest.
    pub locations: Vec<Location>,
    /// Command-mode guest identity, if any.
    pub command_guest: Option<GuestId>,
}

/// Connected host runtime: registry, argv, mounts, and backend bundle.
///
/// A thin handle over shared state: `clone()` bumps two reference counts, so
/// the per-request and per-message handler clones never copy the backend
/// bundle.
pub struct Runtime<B: 'static> {
    inner: Arc<RuntimeInner<B>>,
    // Cached host→guest dispatch capability, built once per runtime so
    // `store()` hands out clones instead of allocating one per store.
    dispatcher: Arc<dyn Dispatcher>,
}

struct RuntimeInner<B: 'static> {
    // Deployment name read by trigger servers and the bootstrap log —
    // carried state, never a process environment variable.
    name: Arc<str>,
    registry: Arc<Registry<StoreCtx<B>>>,
    args: Arc<Vec<String>>,
    mounts: Arc<MountRegistry>,
    backends: B,
    // Command-mode guest identity; absent, command mode routes to
    // the sole static `wasi:cli/run` exporter.
    command_guest: Option<GuestId>,
    // The manifest's plugin acquisition locations, read by the loader
    // capability's install.
    locations: Vec<Location>,
    // Capability-crate state installed by the extend hook and
    // shared with every store context.
    extensions: Extensions,
}

/// [`Dispatcher`] over the runtime's shared state.
///
/// A separate type (rather than `Runtime` itself) so the cached
/// `Arc<dyn Dispatcher>` inside [`Runtime`] does not create a reference cycle.
pub struct RuntimeDispatcher<B: 'static> {
    inner: Arc<RuntimeInner<B>>,
}

impl<B: Clone + Send + Sync + 'static> RuntimeDispatcher<B> {
    /// Rehydrate a full runtime handle for a dispatched call.
    pub fn runtime(&self) -> Runtime<B> {
        Runtime::with_inner(Arc::clone(&self.inner))
    }
}

/// A non-owning [`Runtime`] handle, the form a runtime extension holds to
/// call back into the runtime without leaking it through a reference cycle.
pub struct WeakRuntime<B: 'static> {
    inner: Weak<RuntimeInner<B>>,
}

// Manual: a handle clone must not require `B: Clone`.
impl<B: 'static> Clone for WeakRuntime<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Weak::clone(&self.inner),
        }
    }
}

impl<B: Clone + Send + Sync + 'static> WeakRuntime<B> {
    /// Upgrade to a full handle; `None` once the runtime has shut down.
    #[must_use]
    pub fn upgrade(&self) -> Option<Runtime<B>> {
        Some(Runtime::with_inner(self.inner.upgrade()?))
    }
}

/// Why [`Runtime::admit`] refused a late guest; each variant carries the
/// refusal's description.
#[derive(Clone, Debug)]
pub enum AdmitError {
    /// The bytes are a native artifact, not a valid raw wasm component, or
    /// failed pre-instantiation against the deployment's host set.
    ArtifactRefused(String),
    /// The identity is already registered — an earlier or racing
    /// registration holds it.
    AlreadyRegistered(String),
    /// Serve wiring or publication failed.
    Internal(String),
}

impl fmt::Display for AdmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArtifactRefused(reason)
            | Self::AlreadyRegistered(reason)
            | Self::Internal(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for AdmitError {}

// Manual: `StoreCtx<B>` is not `Clone`; both fields are `Arc`-backed.
impl<B: Clone + Send + Sync + 'static> Clone for Runtime<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            dispatcher: Arc::clone(&self.dispatcher),
        }
    }
}

impl<B: Clone + Send + Sync + 'static> Runtime<B> {
    fn with_inner(inner: Arc<RuntimeInner<B>>) -> Self {
        let dispatcher = Arc::new(RuntimeDispatcher {
            inner: Arc::clone(&inner),
        });
        Self { inner, dispatcher }
    }

    /// Build a runtime from already-assembled parts.
    ///
    /// Does not wire the host-mediated link serve side — a caller whose
    /// deployment declares link interfaces must run
    /// [`serve_links`](Self::serve_links) itself before dispatching.
    #[must_use]
    pub fn from_parts(parts: RuntimeParts<B>) -> Self {
        Self::with_inner(Arc::new(RuntimeInner {
            name: parts.name,
            registry: parts.registry,
            args: Arc::new(parts.args),
            mounts: parts.mounts,
            backends: parts.backends,
            command_guest: parts.command_guest,
            locations: parts.locations,
            extensions: Extensions::new(),
        }))
    }

    /// The deployment's plugin acquisition locations (the manifest's
    /// `[[plugin.location]]` entries), for the loader capability to install against.
    #[must_use]
    pub fn plugin_locations(&self) -> &[Location] {
        &self.inner.locations
    }

    /// The deployment name — read by trigger servers and the bootstrap log.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Build the HTTP trigger's [`TriggerRouter`] over this runtime's
    /// registry and static route table so the boot-time routing decision
    /// lives in one place.
    ///
    /// `probe` resolves a guest's typed handler indices; a guest is capable
    /// exactly when it succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error if a route names a guest that does not export the
    /// handler, or two or more guests export it with no routes.
    pub fn http_trigger_router<I, E, F>(&self, probe: F) -> Result<TriggerRouter<I, HttpRoutes>>
    where
        F: FnMut(&InstancePre<StoreCtx<B>>) -> std::result::Result<I, E>,
    {
        TriggerRouter::build(
            self.registry(),
            "http",
            self.registry().routes().http().clone(),
            probe,
        )
    }

    /// The command-mode guest identity (the manifest entry marked
    /// `command = true`), if any.
    #[must_use]
    pub fn command_guest(&self) -> Option<&GuestId> {
        self.inner.command_guest.as_ref()
    }

    /// Guest registry.
    #[must_use]
    pub fn registry(&self) -> &Registry<StoreCtx<B>> {
        &self.inner.registry
    }

    /// The deployment's connected backend bundle.
    #[must_use]
    pub fn backends(&self) -> &B {
        &self.inner.backends
    }

    /// The capability-crate state installed by the extend hook —
    /// the same set every store context carries.
    #[must_use]
    pub fn extensions(&self) -> &Extensions {
        &self.inner.extensions
    }

    /// A non-owning handle for state that must call back into the runtime;
    /// see [`Extensions`] for why extensions never hold a [`Runtime`].
    #[must_use]
    pub fn downgrade(&self) -> WeakRuntime<B> {
        WeakRuntime {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// The cached host→guest dispatch capability — the same handle
    /// every store context carries, for host-side callers (tests,
    /// embedders) that invoke a guest export directly.
    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn Dispatcher> {
        Arc::clone(&self.dispatcher)
    }

    /// Runtime options from the environment.
    #[must_use]
    pub fn options(&self) -> &RuntimeOptions {
        self.registry().options()
    }

    /// Fresh per-guest store context.
    #[must_use]
    pub fn store(&self) -> StoreCtx<B> {
        StoreCtx {
            base: StoreBase::new(crate::StoreConfig {
                options: self.options(),
                dispatcher: Arc::clone(&self.dispatcher),
                args: Some(Arc::clone(&self.inner.args)),
                mounts: Some(Arc::clone(&self.inner.mounts)),
                env: None,
                extensions: self.inner.extensions.clone(),
            }),
            backends: self.inner.backends.clone(),
        }
    }

    /// Store with epoch deadline, optional fuel, and memory limiter installed.
    ///
    /// # Panics
    ///
    /// Panics if `MAX_FUEL` is set but the engine was built without fuel
    /// metering — a configuration mismatch that would otherwise run guests
    /// unmetered.
    #[must_use]
    pub fn build_store(&self, data: StoreCtx<B>) -> Store<StoreCtx<B>> {
        let options = self.options();
        let mut store = Store::new(self.registry().engine(), data);

        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

        if options.max_fuel > 0 {
            // `Config::from(&options)` enables `consume_fuel` whenever
            // `max_fuel > 0`, so a failure here means the engine was built
            // from different options; running unmetered would silently void
            // the fuel bound.
            store.set_fuel(options.max_fuel).expect("engine was built without fuel metering");
        }

        store.limiter(|ctx| ctx.limits());
        store
    }

    /// A shareable factory for fresh, fully configured guest stores — what the
    /// link serve side hands to each served function to instantiate the
    /// target per call.
    #[must_use]
    pub fn store_factory(&self) -> Arc<dyn Fn() -> Store<StoreCtx<B>> + Send + Sync> {
        let runtime = self.clone();
        Arc::new(move || runtime.build_store(runtime.store()))
    }

    /// Instantiate a guest component into `store`.
    ///
    /// # Errors
    ///
    /// Returns an error if the component cannot be instantiated.
    pub async fn instantiate(
        &self, instance_pre: &InstancePre<StoreCtx<B>>, store: &mut Store<StoreCtx<B>>,
    ) -> Result<Instance> {
        let instance = instance_pre.instantiate_async(store).await?;
        tracing::debug!("component instantiated");
        Ok(instance)
    }

    /// Drive the deployment's `wasi:cli/run` command once, returning the
    /// guest's exit status.
    ///
    /// # Errors
    ///
    /// Returns an error if the command guest is not registered, routing is
    /// ambiguous, the guest cannot be instantiated, or the command traps
    /// without a guest exit code.
    pub async fn run_command(&self) -> Result<ExitStatus> {
        command::drive(self).await
    }

    /// Register a guest at run time: load `artifact`, pre-instantiate it
    /// against the shared host set, wire its host-mediated link serve side,
    /// then publish entry and endpoint as one atomic lifecycle transition —
    /// no dispatch can ever resolve the entry and miss the endpoint, or vice
    /// versa.
    ///
    /// The identity is opaque and must not already be registered; an upgrade
    /// is [`deregister`](Self::deregister) + `register` (or a new id). A
    /// failed registration leaves no partial state.
    ///
    /// # Errors
    ///
    /// Returns an error if `id` is already registered, the artifact cannot be
    /// loaded, the component's imports exceed the deployment's linked host set
    /// and declared link interfaces, or its linked exports cannot be served.
    pub async fn register(&self, id: impl Into<GuestId>, artifact: GuestArtifact) -> Result<()> {
        let id = id.into();
        let registry = self.registry();

        // Early occupancy check to skip the load/serve work; the publish below
        // re-checks transactionally, so a racing registration cannot slip in.
        anyhow::ensure!(registry.get(&id).is_none(), "guest `{id}` is already registered");

        let component = artifact
            .load(registry.engine())
            .await
            .with_context(|| format!("loading guest `{id}`"))?;
        self.register_component(id, component).await
    }

    /// [`register`](Self::register) internals over an already-loaded
    /// component.
    async fn register_component(&self, id: GuestId, component: Component) -> Result<()> {
        let registry = self.registry();
        let instance_pre = registry.instantiate_late(&id, &component)?;
        let guest = Guest::local(id.clone(), instance_pre);

        // Serve the guest's linked exports (if any) as a pending endpoint;
        // publish then makes the endpoint and the registry entry observable in
        // one atomic step, discarding the endpoint if a racing registration won.
        registry
            .seam()
            .serve(self.store_factory(), &guest)
            .await
            .with_context(|| format!("serving guest `{id}` link exports"))?;
        registry.publish(guest).map_err(PublishError::into_anyhow)?;

        tracing::debug!(guest = %id, "guest registered");
        Ok(())
    }

    /// Admit raw wasm bytes as a late guest: refuse a native (pre-compiled)
    /// artifact before wasmtime sees the bytes, validate on the safe path,
    /// then register and serve the component under `id` — the privileged
    /// registration half behind the `omnia:plugins/loader` capability.
    /// Acquisition, digest policy, and idempotency live with the loader
    /// (`omnia-plugin`). Whether the component exports a linked interface is
    /// not checked here: a guest that exports none is still reachable through
    /// the host [`Dispatcher`], and a link call to it fails at the call site.
    ///
    /// The registry entry records the content digest of `bytes`, so the
    /// attestation lives exactly as long as the entry —
    /// [`Guest::digest`](crate::Guest::digest) reads it back.
    ///
    /// # Errors
    ///
    /// Returns a typed [`AdmitError`] naming the refusal: refused artifact,
    /// an identity already registered (an earlier or racing registration), or
    /// an internal serve/publication failure.
    pub async fn admit(&self, id: GuestId, bytes: Vec<u8>) -> Result<(), AdmitError> {
        let digest: std::sync::Arc<str> = std::sync::Arc::from(crate::sha256_digest(&bytes));

        // Safe validation plus sandboxed JIT — the explicitly safe constructor.
        let component =
            GuestArtifact::wasm(bytes).load(self.registry().engine()).await.map_err(|error| {
                AdmitError::ArtifactRefused(format!("validating `{id}`: {error:#}"))
            })?;

        // The same publish sequence as `Runtime::register`: pre-instantiate
        // against the shared host set, wire seam exports, publish atomically.
        let instance_pre = self.registry().instantiate_late(&id, &component).map_err(|error| {
            AdmitError::ArtifactRefused(format!("pre-instantiating `{id}`: {error:#}"))
        })?;
        let guest = Guest::local(id.clone(), instance_pre).with_digest(digest);
        self.registry().seam().serve(self.store_factory(), &guest).await.map_err(|error| {
            AdmitError::Internal(format!("serving `{id}` seam exports: {error:#}"))
        })?;
        self.registry().publish(guest).map_err(|error| match error {
            PublishError::Occupied(id) => {
                AdmitError::AlreadyRegistered(format!("guest `{id}` is already registered"))
            }
            PublishError::Transport(error) => {
                AdmitError::Internal(format!("publishing `{id}`: {error:#}"))
            }
        })?;

        tracing::debug!(guest = %id, "late guest admitted");
        Ok(())
    }

    /// Remove a dynamically registered guest. New dispatches to `id` fail as
    /// unregistered; in-flight calls complete on the instance they hold
    /// (instance-per-call). Static deployment entries are refused.
    ///
    /// # Errors
    ///
    /// Returns an error if `id` names a static `[[guest]]` entry or is not
    /// registered.
    pub fn deregister(&self, id: &GuestId) -> Result<()> {
        self.registry().remove(id)?;
        tracing::debug!(guest = %id, "guest deregistered");
        Ok(())
    }

    /// Release every link-serve endpoint, aborting the drain tasks that pin
    /// `Runtime` clones (and with them the engine's pooling reservation).
    ///
    /// `run` does this as the drive completes; an embedder holding a
    /// [`from_parts`](Self::from_parts) runtime calls it when the deployment
    /// is finished. In-flight invocations hold their own server handles and
    /// complete; only new dispatches are cut off.
    pub fn shutdown(&self) {
        self.registry().seam().shutdown();
    }

    /// Wire the serve side of every registered guest's linked exports, then
    /// publish them all under one lifecycle transition so polyfilled imports
    /// can reach them. `Deployment::assemble` calls this during bootstrap; only
    /// a runtime assembled through [`from_parts`](Self::from_parts) wires it
    /// explicitly. A no-op for a deployment that declares no link interfaces.
    ///
    /// # Errors
    ///
    /// Returns an error if a guest's export cannot be served, or a served guest
    /// already has an endpoint (`serve_links` ran twice).
    pub async fn serve_links(&self) -> Result<()> {
        let registry = self.registry();
        let seam = registry.seam();
        let factory = self.store_factory();
        let guests: Vec<_> = registry.guests().collect();

        // On any failure, release what is still parked so a failed bootstrap
        // pins nothing.
        let discard_from = |first_unpublished: usize| {
            for guest in &guests[first_unpublished..] {
                seam.discard(guest.id());
            }
        };

        for guest in &guests {
            if let Err(error) = seam.serve(Arc::clone(&factory), guest).await {
                discard_from(0);
                return Err(error);
            }
        }

        let _lifecycle = registry.lifecycle_write();
        for (published, guest) in guests.iter().enumerate() {
            if let Err(error) = seam.publish(guest.id()) {
                discard_from(published + 1);
                return Err(error);
            }
        }
        Ok(())
    }
}

/// Wire the link serve side of every registered guest; see
/// [`Runtime::serve_links`].
///
/// # Errors
///
/// Returns an error if a guest's export cannot be served, or a served guest
/// already has an endpoint.
pub async fn serve_links<B>(runtime: &Runtime<B>) -> Result<()>
where
    B: Clone + Send + Sync + 'static,
{
    runtime.serve_links().await
}

#[cfg(test)]
mod tests {
    use super::ExitStatus;

    #[test]
    fn code_u8_low_byte() {
        // The POSIX low-byte truncation is the only non-trivial ExitStatus logic.
        assert_eq!(ExitStatus::from(256).code_u8(), 0);
        assert_eq!(ExitStatus::from(257).code_u8(), 1);
        assert_eq!(ExitStatus::from(-1).code_u8(), 255);
    }
}
