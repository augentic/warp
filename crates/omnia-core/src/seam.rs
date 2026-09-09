//! The seam between the guest registry and guest→guest linking.
//!
//! The registry owns guest lifecycle; how a guest's linked imports are
//! satisfied and its linked exports served is behind [`LinkSeam`]. A
//! deployment that declares no link interfaces installs [`NoLinks`]; one that
//! does installs an implementation that polyfills, serves, and routes calls
//! over a transport.

use std::sync::Arc;

use anyhow::Result;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::artifact::LoadedGuest;
use crate::host::FutureResult;
use crate::registry::{Guest, GuestId};

/// Builds a fresh, fully configured guest store per served invocation.
pub type StoreFactory<T> = Arc<dyn Fn() -> Store<T> + Send + Sync>;

/// Guest→guest linking as the registry drives it.
///
/// Endpoints move through two stages. [`serve`](Self::serve) runs *outside*
/// the registry's lifecycle gate and writes only pending state;
/// [`publish`](Self::publish), [`discard`](Self::discard) and
/// [`remove`](Self::remove) run *under* the gate's write guard, so a guest's
/// registry entry and its live endpoint change as one step. A call path never
/// reads pending state and reads live state under the seam's own lock, not the
/// gate: a call racing a deregister may complete against the departing
/// instance, exactly as an in-flight invocation does.
pub trait LinkSeam<T: 'static>: Send + Sync + 'static {
    /// Polyfill the declared link imports of every bootstrap guest onto the
    /// shared linker, before pre-instantiation.
    ///
    /// # Errors
    ///
    /// Returns an error if a linked import cannot be polyfilled.
    fn polyfill(
        &self, engine: &Engine, linker: &mut Linker<T>, guests: &[LoadedGuest],
    ) -> Result<()>;

    /// Polyfill a late guest's remaining link imports onto `linker`, a clone
    /// of the shared linker.
    ///
    /// # Errors
    ///
    /// Returns an error if a linked import cannot be polyfilled.
    fn polyfill_late(
        &self, engine: &Engine, linker: &mut Linker<T>, id: &GuestId, component: &Component,
    ) -> Result<()>;

    /// Serve `guest`'s linked exports, parking the result pending until it is
    /// published or discarded.
    fn serve(&self, factory: StoreFactory<T>, guest: &Guest<T>) -> FutureResult<()>;

    /// Move `id`'s pending endpoint live; a no-op when nothing is pending.
    ///
    /// # Errors
    ///
    /// Returns an error if `id` already has a live endpoint.
    fn publish(&self, id: &GuestId) -> Result<()>;

    /// Drop `id`'s pending endpoint.
    fn discard(&self, id: &GuestId);

    /// Drop `id`'s live endpoint; in-flight invocations complete.
    fn remove(&self, id: &GuestId);

    /// Drop every pending and live endpoint.
    fn shutdown(&self);
}

/// The seam of a deployment that declares no link interfaces.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLinks;

impl<T: 'static> LinkSeam<T> for NoLinks {
    fn polyfill(
        &self, _engine: &Engine, _linker: &mut Linker<T>, _guests: &[LoadedGuest],
    ) -> Result<()> {
        Ok(())
    }

    fn polyfill_late(
        &self, _engine: &Engine, _linker: &mut Linker<T>, _id: &GuestId, _component: &Component,
    ) -> Result<()> {
        Ok(())
    }

    fn serve(&self, _factory: StoreFactory<T>, _guest: &Guest<T>) -> FutureResult<()> {
        Box::pin(async { Ok(()) })
    }

    fn publish(&self, _id: &GuestId) -> Result<()> {
        Ok(())
    }

    fn discard(&self, _id: &GuestId) {}

    fn remove(&self, _id: &GuestId) {}

    fn shutdown(&self) {}
}
