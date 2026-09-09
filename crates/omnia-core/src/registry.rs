//! # Guest registry
//!
//! One [`Engine`] and one `Linker` hold many pre-instantiated guests at once,
//! each selectable by an opaque [`GuestId`]. A registry entry is instantiated
//! fresh per call and discarded (instance-per-call). This is pure wasmtime
//! infrastructure: it is what lets one process route an HTTP request, a CLI
//! command, and a topic message to *different* guests.
//!
//! The runtime core treats identities as opaque keys; consumers project their own
//! scheme onto them. Omnia never parses a [`GuestId`].

mod routing;

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use anyhow::{Context as _, Result, bail, ensure};
pub use routing::{CliRoutes, HttpRoutes, PatternRoutes, Resolver, Routes, TriggerRouter};
use wasmtime::Engine;
use wasmtime::component::{Component, InstancePre, Linker};
use wasmtime_wasi::WasiView;

use crate::RuntimeOptions;
use crate::artifact::LoadedGuest;
use crate::seam::LinkSeam;

/// Opaque guest identity.
///
/// The runtime core treats it as an ordered string key; consumers (e.g. Specify)
/// project their own scheme onto it (`source:typescript`, ...). Omnia never
/// parses it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GuestId(Arc<str>);

impl GuestId {
    /// Returns the identity as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GuestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for GuestId {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for GuestId {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

/// A registry entry's resolution target.
///
/// Only [`Target::Local`] exists today; a remote wRPC-endpoint variant will land
/// with distributed transport.
enum Target<T: 'static> {
    /// A locally pre-instantiated component.
    Local(InstancePre<T>),
}

/// A registered guest: an opaque identity bound to a resolution target.
pub struct Guest<T: 'static> {
    id: GuestId,
    target: Target<T>,
    // Content digest of the admitted bytes; `None` for assemble-time and
    // `register`-path entries, whose bytes the registry never hashed.
    digest: Option<Arc<str>>,
}

impl<T: 'static> Guest<T> {
    /// Create a guest backed by a local pre-instantiated component.
    #[must_use]
    pub const fn local(id: GuestId, instance_pre: InstancePre<T>) -> Self {
        Self {
            id,
            target: Target::Local(instance_pre),
            digest: None,
        }
    }

    /// Record the `sha256:<hex>` content digest of the guest's bytes, so the
    /// attestation lives and dies with the registry entry itself.
    #[must_use]
    pub fn with_digest(mut self, digest: impl Into<Arc<str>>) -> Self {
        self.digest = Some(digest.into());
        self
    }

    /// Returns the guest's identity.
    #[must_use]
    pub const fn id(&self) -> &GuestId {
        &self.id
    }

    /// The recorded `sha256:<hex>` content digest, if the guest was admitted
    /// from hashed bytes.
    #[must_use]
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    /// Returns the guest's pre-instantiated component, ready to instantiate
    /// fresh on a new [`wasmtime::Store`] per call.
    #[must_use]
    pub const fn instance_pre(&self) -> &InstancePre<T> {
        match &self.target {
            Target::Local(pre) => pre,
        }
    }

    /// Returns the underlying component, used to introspect a guest's exported
    /// interfaces when wiring the host-mediated link serve side.
    #[must_use]
    pub fn component(&self) -> &Component {
        self.instance_pre().component()
    }
}

/// One [`Engine`] + one `Linker`; many pre-instantiated guests keyed by
/// identity.
///
/// Every guest is pre-instantiated against the *same* linker, so they share one
/// set of host interfaces and one pooling pool — load-bearing for the
/// instance-per-call cost story. Pre-instantiation happens once, at
/// registration; per call only a fresh instantiate on a new store remains.
///
/// The guest map grows (and shrinks) after assembly through the dynamic
/// registration seam ([`Runtime::register`](crate::Runtime::register)); the
/// linker is retained so late guests pre-instantiate against the same host set.
///
/// The registry is cheap to share behind an `Arc`, matching how the runtime
/// context is cloned into each connection handler.
pub struct Registry<T: 'static> {
    engine: Engine,
    options: RuntimeOptions,
    linker: Linker<T>,
    // Concurrent-read, exclusive-write; guards are never held across an await.
    guests: RwLock<BTreeMap<GuestId, Arc<Guest<T>>>>,
    // Serializes guest lifecycle transitions (register/deregister/bootstrap
    // serve wiring) against readers, so the guest map and the seam's live
    // endpoints always change as one atomic step. Lock order: this gate
    // first, then a single inner map — never the other way around, and never
    // across an await.
    lifecycle: RwLock<()>,
    // Assemble-time identities, which deregistration refuses to remove.
    static_ids: BTreeSet<GuestId>,
    routes: Routes,
    seam: Arc<dyn LinkSeam<T>>,
}

impl<T: WasiView + 'static> Registry<T> {
    /// Assemble a registry from a linked deployment's parts: polyfill link
    /// imports through `seam`, pre-instantiate every loaded guest, validate
    /// that routes name registered guests, and freeze the static set.
    ///
    /// # Errors
    ///
    /// Returns an error if there are no guests to register (unless
    /// `allow_empty`), link imports cannot be polyfilled, a component cannot
    /// be pre-instantiated, or a route targets a guest that is not registered.
    pub fn assemble(
        engine: Engine, mut linker: Linker<T>, options: RuntimeOptions, loaded: Vec<LoadedGuest>,
        routes: Routes, seam: Arc<dyn LinkSeam<T>>, allow_empty: bool,
    ) -> Result<Self> {
        if loaded.is_empty() && !allow_empty {
            bail!("cannot build a guest registry with no guests");
        }

        seam.polyfill(&engine, &mut linker, &loaded)?;

        let mut guests = BTreeMap::new();
        for guest in loaded {
            let instance_pre = linker
                .instantiate_pre(&guest.component)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("pre-instantiating guest `{}`", guest.id))?;
            let id = guest.id.clone();
            if guests
                .insert(guest.id.clone(), Arc::new(Guest::local(guest.id, instance_pre)))
                .is_some()
            {
                bail!("duplicate guest id `{id}`: guest identities must be unique");
            }
        }

        for target in routes.targets() {
            if !guests.contains_key(target) {
                bail!("route targets guest `{target}`, which is not registered");
            }
        }

        tracing::debug!(guests = guests.len(), "runtime initialized");

        let static_ids = guests.keys().cloned().collect();
        Ok(Self {
            engine,
            options,
            linker,
            guests: RwLock::new(guests),
            lifecycle: RwLock::new(()),
            static_ids,
            routes,
            seam,
        })
    }

    /// Pre-instantiate a late (dynamically registered) component against the
    /// shared host set.
    ///
    /// Link functions the bootstrap did not polyfill (no static guest imports
    /// them) are polyfilled by the seam on a clone of the retained linker,
    /// from this component's own import types — the shared linker is never
    /// mutated after bootstrap. Imports outside the linked host set and the
    /// declared link interfaces fail here, exactly as at bootstrap.
    pub(crate) fn instantiate_late(
        &self, id: &GuestId, component: &Component,
    ) -> Result<InstancePre<T>> {
        let mut linker = self.linker.clone();
        self.seam.polyfill_late(&self.engine, &mut linker, id, component)?;
        linker
            .instantiate_pre(component)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("pre-instantiating guest `{id}`"))
    }
}

impl<T: 'static> Registry<T> {
    /// Returns the shared engine every guest is instantiated against.
    #[must_use]
    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Returns the runtime options.
    #[must_use]
    pub const fn options(&self) -> &RuntimeOptions {
        &self.options
    }

    /// Enter a lifecycle read section: registry lookups taken under
    /// this guard never observe a half-applied register or deregister.
    fn lifecycle_read(&self) -> RwLockReadGuard<'_, ()> {
        self.lifecycle.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enter a lifecycle write section: the holder may mutate the guest map
    /// and the seam's live endpoints as one atomic transition.
    pub(crate) fn lifecycle_write(&self) -> RwLockWriteGuard<'_, ()> {
        self.lifecycle.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Look up a guest by identity.
    #[must_use]
    pub fn get(&self, id: &GuestId) -> Option<Arc<Guest<T>>> {
        let _lifecycle = self.lifecycle_read();
        self.guests.read().unwrap_or_else(PoisonError::into_inner).get(id).cloned()
    }

    /// Snapshot every registered guest in a deterministic, identity-sorted
    /// order so per-trigger capability and ambiguity errors are stable across
    /// runs.
    ///
    /// The order falls out of the [`BTreeMap`] keying; no per-call sort.
    pub fn guests(&self) -> impl ExactSizeIterator<Item = Arc<Guest<T>>> {
        let snapshot: Vec<Arc<Guest<T>>> = {
            let _lifecycle = self.lifecycle_read();
            self.guests.read().unwrap_or_else(PoisonError::into_inner).values().cloned().collect()
        };
        snapshot.into_iter()
    }

    /// Publish a late guest and its pending link endpoint (parked by the serve
    /// side) as one lifecycle transition. Refuses an identity that is already
    /// registered (static entries can never be shadowed; a dynamic upgrade is
    /// deregister + register), discarding the pending endpoint; on refusal the
    /// registry map is untouched, so a failed registration leaves no partial
    /// state.
    pub(crate) fn publish(&self, guest: Guest<T>) -> Result<(), PublishError> {
        let id = guest.id().clone();

        // Lifecycle write first, then the inner maps (the crate-wide order).
        let _lifecycle = self.lifecycle_write();
        let mut guests = self.guests.write().unwrap_or_else(PoisonError::into_inner);
        match guests.entry(id.clone()) {
            btree_map::Entry::Occupied(_) => {
                self.seam.discard(&id);
                return Err(PublishError::Occupied(id));
            }
            btree_map::Entry::Vacant(slot) => {
                // Seam before entry: `publish` refuses an occupied live slot,
                // and failing here leaves the registry map untouched.
                self.seam.publish(&id).map_err(PublishError::Transport)?;
                slot.insert(Arc::new(guest));
            }
        }
        drop(guests);
        Ok(())
    }

    /// Remove a dynamically registered guest and its link endpoint as one
    /// lifecycle transition. Refuses static (assemble-time) entries and
    /// unregistered identities.
    pub(crate) fn remove(&self, id: &GuestId) -> Result<()> {
        if self.static_ids.contains(id) {
            bail!("guest `{id}` is a static deployment entry and cannot be deregistered");
        }

        let _lifecycle = self.lifecycle_write();
        let removed = self.guests.write().unwrap_or_else(PoisonError::into_inner).remove(id);
        ensure!(removed.is_some(), "guest `{id}` is not registered");
        self.seam.remove(id);
        Ok(())
    }

    /// Returns the per-trigger inbound route tables built from the manifest
    /// guests' `routes` lists.
    #[must_use]
    pub const fn routes(&self) -> &Routes {
        &self.routes
    }

    /// Returns the guest→guest link seam the registry drives.
    #[must_use]
    pub(crate) const fn seam(&self) -> &Arc<dyn LinkSeam<T>> {
        &self.seam
    }

    /// Returns the number of registered guests.
    #[must_use]
    pub fn len(&self) -> usize {
        let _lifecycle = self.lifecycle_read();
        self.guests.read().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Returns `true` if the registry has no guests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let _lifecycle = self.lifecycle_read();
        self.guests.read().unwrap_or_else(PoisonError::into_inner).is_empty()
    }
}

/// Why [`Registry::publish`] refused a late guest; `Occupied` is distinct so
/// callers can treat an identity conflict as a race, not a fault.
#[derive(Debug)]
pub enum PublishError {
    /// The identity is already registered.
    Occupied(GuestId),
    /// Link-endpoint installation failed.
    Transport(anyhow::Error),
}

impl PublishError {
    pub(crate) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Occupied(id) => anyhow::anyhow!("guest `{id}` is already registered"),
            Self::Transport(error) => error,
        }
    }
}
