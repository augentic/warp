//! Per-store context. [`StoreBase`] holds the state identical for every
//! deployment (WASI table/context, memory limiter, host→guest dispatcher, and
//! — under the `wrpc` feature — wRPC view state); [`StoreCtx`] pairs it with
//! the deployment's backend bundle `B` and implements the fixed
//! `WasiView`/`WrpcView`/`HasLimits` views, plus the generic [`StoreView`]
//! blanket every host's `add_to_linker` accessor rides
//! (`StoreView<H> for StoreCtx<B> where B: Provides<H>`).

#[cfg(feature = "wrpc")]
mod wrpc;

use std::sync::Arc;

use wasmtime::component::HasData;
use wasmtime::{StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{FsPerms, ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtxView, WasiHttpView};
#[cfg(feature = "wrpc")]
use wrpc_wasmtime::{WrpcCtxView, WrpcView};

#[cfg(feature = "wrpc")]
pub use self::wrpc::{LinkClient, WrpcState};
use crate::{Dispatcher, Extensions, HostCtx, MountRegistry, Provides, RuntimeOptions};

/// Exposes a store context's [`StoreLimits`] so the runtime can install a
/// per-guest resource limiter on every [`Store`](wasmtime::Store) it creates.
pub trait HasLimits {
    /// Returns a mutable reference to the context's resource limits.
    fn limits(&mut self) -> &mut StoreLimits;
}

/// The per-store construction inputs for [`StoreBase::new`].
///
/// `options` and `dispatcher` are required; the rest default sensibly (empty
/// argv, no mounts, host env inheritance) so hand-written test runtimes build
/// unchanged.
pub struct StoreConfig<'a> {
    /// Runtime options; caps linear-memory growth at
    /// [`RuntimeOptions::max_memory_bytes`].
    pub options: &'a RuntimeOptions,
    /// Type-erased host->guest dispatcher: a fresh handle to the owning
    /// [`Runtime`](crate::Runtime) so any host->guest call lands a new
    /// instance.
    pub dispatcher: Arc<dyn Dispatcher>,
    /// Guest argv (`args[0]` is the program name); `None` for reactor
    /// deployments that do not model a CLI invocation.
    pub args: Option<Arc<Vec<String>>>,
    /// Mount registry preopened into the guest sandbox; `None` for
    /// deployments without `[[mount]]`s.
    pub mounts: Option<Arc<MountRegistry>>,
    /// Complete guest environment replacing host inheritance; `None` inherits
    /// the host env.
    pub env: Option<Arc<Vec<(String, String)>>>,
    /// The runtime's installed extensions; [`Extensions::new`] for hand-built
    /// store contexts, where a capability that reads its extension refuses.
    pub extensions: Extensions,
}

/// The fixed per-store state shared by every guest store context.
///
/// Construction policy (WASI inheritance, argv, the memory limit, and inert wRPC
/// view state) lives in [`StoreBase::new`] so it is documented and
/// unit-testable instead of being inlined in [`Runtime::store`](crate::Runtime::store).
pub struct StoreBase {
    /// The store's WASI resource table.
    pub table: ResourceTable,
    /// The store's WASI context (inherited env/stdin, host stdout/stderr).
    pub wasi: WasiCtx,
    /// The per-guest memory limiter the runtime installs on every [`Store`].
    ///
    /// [`Store`]: wasmtime::Store
    pub limits: StoreLimits,
    /// Per-store wRPC view state for host-mediated dynamic linking; inert
    /// unless the deployment declares link interfaces (the manifest `[link]
    /// interfaces` list). Present only under the `wrpc` feature.
    #[cfg(feature = "wrpc")]
    pub wrpc: WrpcState,
    /// Type-erased host->guest dispatcher; a fresh handle to the owning
    /// runtime. Inert unless a host binding reaches for it.
    pub dispatcher: Arc<dyn Dispatcher>,
    /// Mount registry: the startup-validated mounts also preopened into
    /// [`wasi`](Self::wasi). A consuming host crate reads it to match a lent
    /// `descriptor` back to its mount by directory identity. Empty unless the
    /// deployment configures `[[mount]]`s.
    pub mounts: Arc<MountRegistry>,
    /// Handle to the runtime's installed extensions, the state slot a
    /// capability crate's host binding reaches for. Empty in hand-built
    /// store contexts.
    pub extensions: Extensions,
}

impl StoreBase {
    /// Build the fixed per-store state for a single guest invocation, applying
    /// the WASI construction policy shared by every deployment.
    ///
    /// Applies the guest environment (the explicit [`env`](StoreConfig::env)
    /// list when set, host inheritance otherwise), inherits stdin, wires
    /// stdout/stderr to the host streams, applies the configured argv, caps
    /// linear-memory growth, and creates fresh, inert wRPC view state.
    #[must_use]
    pub fn new(config: StoreConfig<'_>) -> Self {
        let mounts = config.mounts.unwrap_or_default();

        let mut wasi_builder = WasiCtxBuilder::new();
        match &config.env {
            Some(env) => {
                wasi_builder.envs(env);
            }
            None => {
                wasi_builder.inherit_env();
            }
        }
        wasi_builder.inherit_stdin().stdout(tokio::io::stdout()).stderr(tokio::io::stderr());
        if let Some(args) = &config.args {
            wasi_builder.args(args.as_slice());
        }

        // Preopen each authorized mount into the guest sandbox. The
        // registry was opened + validated once at startup, so a failure here is
        // rare (e.g. a mount removed mid-run); log and skip — the guest simply
        // can't lend that tree and the consuming host's identity match then
        // fails cleanly, with no ambient fallback.
        for entry in mounts.entries() {
            let perms = if entry.writable { FsPerms::ReadWrite } else { FsPerms::ReadOnly };
            if let Err(error) = wasi_builder.preopened_dir(&entry.host_path, &entry.name, perms) {
                tracing::warn!(
                    %error,
                    name = %entry.name,
                    path = %entry.host_path.display(),
                    "failed to preopen mount; guest will not see it",
                );
            }
        }

        Self {
            table: ResourceTable::new(),
            wasi: wasi_builder.build(),
            limits: StoreLimitsBuilder::new().memory_size(config.options.max_memory_bytes).build(),
            #[cfg(feature = "wrpc")]
            wrpc: WrpcState::new(),
            dispatcher: config.dispatcher,
            mounts,
            extensions: config.extensions,
        }
    }
}

/// The per-guest store context every deployment shares.
///
/// `StoreCtx<B>` pairs the fixed [`StoreBase`] with the deployment's connected
/// backend bundle `B` — the `runtime!`-generated bundle, or [`()`](unit) for
/// a backend-less deployment (such as a `mode: command` `wasi:cli` runtime). The
/// three fixed views (`WasiView`, `WrpcView`, `HasLimits`) are implemented below
/// against [`base`](Self::base); the generic [`StoreView`] blanket covers every
/// host, so a deployment only supplies the bundle and its [`Provides`] impls
/// (generated by the `runtime!` macro).
///
/// This is the boilerplate the `runtime!` macro and hand-written runtimes
/// previously reproduced per deployment; hosting it here keeps it library code
/// reviewed once.
pub struct StoreCtx<B> {
    /// The fixed per-store state shared by every deployment.
    pub base: StoreBase,
    /// The deployment's connected backend bundle (cloned per store).
    pub backends: B,
}

impl<B: Send + 'static> WasiView for StoreCtx<B> {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.base.wasi,
            table: &mut self.base.table,
        }
    }
}

#[cfg(feature = "wrpc")]
impl<B: Send + 'static> WrpcView for StoreCtx<B> {
    type Invoke = LinkClient;

    fn wrpc(&mut self) -> WrpcCtxView<'_, LinkClient> {
        self.base.wrpc.view(&mut self.base.table)
    }
}

impl<B: Send + 'static> HasLimits for StoreCtx<B> {
    fn limits(&mut self) -> &mut StoreLimits {
        &mut self.base.limits
    }
}

/// The store-side view accessor for host `H`: the one trait every
/// `add_to_linker` accessor rides, replacing the per-host `Wasi*View` traits.
///
/// [`StoreCtx`] gets the blanket impl below through the bundle's
/// [`Provides`] impls; a hand-built store context implements it directly.
pub trait StoreView<H: HostCtx>: Send {
    /// Assemble host `H`'s linker-facing view from this store context.
    fn view(&mut self) -> H::Data<'_>;
}

impl<H: HostCtx, B: Provides<H> + Send + 'static> StoreView<H> for StoreCtx<B> {
    fn view(&mut self) -> H::Data<'_> {
        H::view(self.backends.borrow(), &mut self.base.table)
    }
}

/// The `wasi:http` slot of a backend bundle: split borrows on the backend's
/// hooks and context into the linker-facing view.
pub trait HttpBorrow: Send {
    /// Borrow the `wasi:http` context as the linker-facing view, threading in
    /// the store's [`ResourceTable`].
    fn as_view<'a>(&'a mut self, table: &'a mut ResourceTable) -> WasiHttpCtxView<'a>;
}

/// The [`HostCtx`] carrier for `wasi:http`.
///
/// `wasi:http`'s view trait (`WasiHttpView`) is foreign — re-exported from
/// `wasmtime-wasi-http` — so its blanket impl on `StoreCtx<B>` can only live
/// here, where `StoreCtx` is local, against a carrier this crate can name.
/// Every other host is its own carrier and blankets nothing: its
/// `add_to_linker` rides [`StoreView`] directly.
#[derive(Debug)]
pub struct HttpCtx;

impl HasData for HttpCtx {
    type Data<'a> = WasiHttpCtxView<'a>;
}

impl HostCtx for HttpCtx {
    type Borrow<'a> = &'a mut dyn HttpBorrow;

    fn view<'a>(borrow: Self::Borrow<'a>, table: &'a mut ResourceTable) -> WasiHttpCtxView<'a> {
        borrow.as_view(table)
    }
}

impl<B: Provides<HttpCtx> + Send + 'static> WasiHttpView for StoreCtx<B> {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        <Self as StoreView<HttpCtx>>::view(self)
    }
}

/// Clone-on-read access to a store's startup-validated mount registry.
///
/// Lets a host crate match a lent `wasi:filesystem` descriptor against the
/// store's authorized mounts without carrying the registry on its own view.
pub trait HasMounts: Send {
    /// Clone a handle to the store's mount registry.
    fn mounts(&self) -> Arc<MountRegistry>;
}

impl<B: Send + 'static> HasMounts for StoreCtx<B> {
    fn mounts(&self) -> Arc<MountRegistry> {
        Arc::clone(&self.base.mounts)
    }
}

/// Clone-on-read access to a store's host->guest dispatcher.
///
/// Lets a host crate reach the dispatcher for host-mediated dynamic linking
/// without carrying it on its own view.
pub trait HasDispatcher: Send {
    /// Clone a handle to the store's host->guest dispatcher.
    fn dispatcher(&self) -> Arc<dyn Dispatcher>;
}

impl<B: Send + 'static> HasDispatcher for StoreCtx<B> {
    fn dispatcher(&self) -> Arc<dyn Dispatcher> {
        Arc::clone(&self.base.dispatcher)
    }
}

/// Clone-on-read access to a store's runtime extensions.
///
/// Lets a capability crate's host binding reach the state its
/// extend hook installed without carrying it on
/// its own view.
pub trait HasExtensions: Send {
    /// Clone a handle to the store's runtime extensions.
    fn extensions(&self) -> Extensions;
}

impl<B: Send + 'static> HasExtensions for StoreCtx<B> {
    fn extensions(&self) -> Extensions {
        self.base.extensions.clone()
    }
}
