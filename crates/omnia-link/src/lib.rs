//! # Host-mediated dynamic linking
//!
//! A caller guest imports an interface (say `omnia:link/echo`) whose
//! implementation the host satisfies at runtime. The host polyfills that import
//! on the shared `Linker` so invoking it:
//!
//! 1. extracts a target identity from the call via a [`GuestSelector`],
//! 2. rejects any resource handle attempting to cross the seam,
//! 3. enforces a dispatch-depth bound,
//! 4. instantiates the target *fresh* on a new store and invokes the matching
//!    export over the bound wRPC transport, and
//! 5. returns the typed result, discarding the callee instance.
//!
//! Because step 4 is always a fresh instance, a dispatched call cannot
//! recursively re-enter its caller. The runtime core stays generic: it links whatever
//! interfaces the manifest names, by opaque string, and resolves opaque
//! [`GuestId`]s — it never parses a consumer scheme. The selector runs in the
//! polyfill *before* the call is encoded onto wRPC, so it sees typed
//! parameters. Sync-typed functions are registered with `func_new_async`;
//! async-typed (`async func`) ones with `func_new_concurrent`, whose
//! store-scoped access rules the decode path is built around. See
//! `docs/Architecture.md` (The Guest Registry) for the full design.
//!
//! [`InProcessLinks`] is the [`LinkSeam`] the registry drives when a
//! deployment declares link interfaces.

#![cfg(not(target_arch = "wasm32"))]

mod decode;
mod polyfill;
mod selector;
mod serve;
mod transport;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Result;
use futures::FutureExt as _;
use omnia_core::{ChainPolicy, FutureResult, Guest, GuestId, LinkSeam, LoadedGuest, StoreFactory};
use wasmtime::Engine;
use wasmtime::component::{Component, Linker};
use wasmtime_wasi::WasiView;
use wrpc_wasmtime::WrpcView;

use self::polyfill::{Caller, WiredLinks};
pub use self::selector::{FirstArgSelector, GuestSelector};
pub use self::transport::{InProcess, LinkTransport};

/// Guest→guest linking over the in-process wRPC carrier.
///
/// Holds the selector strategy, the deployment's declared link interfaces, the
/// chain policy (depth and wall-clock bounds), the bound transport, and the
/// functions the bootstrap polyfilled onto the shared linker.
pub struct InProcessLinks {
    selector: Arc<dyn GuestSelector>,
    interfaces: BTreeSet<Box<str>>,
    policy: ChainPolicy,
    transport: InProcess,
    // Link functions polyfilled onto the shared linker at bootstrap, per
    // interface; a late guest's remaining imports are polyfilled on a linker
    // clone against a copy of this map.
    wired: Mutex<WiredLinks>,
}

impl InProcessLinks {
    /// Create the seam for a deployment linking `interfaces` under `policy`.
    /// The transport starts empty; the registry serves and publishes each
    /// guest's endpoint through the [`LinkSeam`] methods.
    #[must_use]
    pub fn new(
        selector: Arc<dyn GuestSelector>, interfaces: BTreeSet<Box<str>>, policy: ChainPolicy,
    ) -> Self {
        Self {
            selector,
            interfaces,
            policy,
            transport: InProcess::default(),
            wired: Mutex::new(WiredLinks::new()),
        }
    }

    /// The deployment's host-mediated link interface names — the set of
    /// interfaces to polyfill (caller side) and serve (callee side).
    #[must_use]
    pub const fn interfaces(&self) -> &BTreeSet<Box<str>> {
        &self.interfaces
    }

    // What every polyfilled import captures for its per-call dispatch.
    fn caller(&self) -> Arc<Caller> {
        Arc::new(Caller {
            selector: Arc::clone(&self.selector),
            policy: self.policy,
            transport: self.transport.clone(),
        })
    }
}

impl<T: WasiView + WrpcView + 'static> LinkSeam<T> for InProcessLinks {
    fn polyfill(
        &self, engine: &Engine, linker: &mut Linker<T>, guests: &[LoadedGuest],
    ) -> Result<()> {
        if self.interfaces.is_empty() {
            return Ok(());
        }
        let caller = self.caller();
        let mut wired = WiredLinks::new();
        for LoadedGuest { id, component } in guests {
            polyfill::polyfill_component(
                engine,
                linker,
                id,
                component,
                &self.interfaces,
                &caller,
                &mut wired,
            )?;
        }
        *self.wired.lock().unwrap_or_else(PoisonError::into_inner) = wired;
        Ok(())
    }

    fn polyfill_late(
        &self, engine: &Engine, linker: &mut Linker<T>, id: &GuestId, component: &Component,
    ) -> Result<()> {
        if self.interfaces.is_empty() {
            return Ok(());
        }
        // A copy: the functions wired here live on a linker clone and must not
        // leak into the bootstrap record.
        let mut wired = self.wired.lock().unwrap_or_else(PoisonError::into_inner).clone();
        polyfill::polyfill_component(
            engine,
            linker,
            id,
            component,
            &self.interfaces,
            &self.caller(),
            &mut wired,
        )
    }

    fn serve(&self, factory: StoreFactory<T>, guest: &Guest<T>) -> FutureResult<()> {
        let transport = self.transport.clone();
        let interfaces = self.interfaces.clone();
        let id = guest.id().clone();
        let instance_pre = guest.instance_pre().clone();
        async move { serve::serve_guest(&transport, &interfaces, factory, &id, instance_pre).await }
            .boxed()
    }

    fn publish(&self, id: &GuestId) -> Result<()> {
        self.transport.publish(id)
    }

    fn discard(&self, id: &GuestId) {
        self.transport.discard(id);
    }

    fn remove(&self, id: &GuestId) {
        self.transport.remove(id);
    }

    fn shutdown(&self) {
        self.transport.clear();
    }
}
