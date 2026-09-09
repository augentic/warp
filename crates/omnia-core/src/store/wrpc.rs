//! Per-store wRPC view state.
//!
//! `wrpc-wasmtime` requires each guest store to expose a [`WrpcCtx`] (a client
//! handle plus a shared-resource table). Omnia's host-mediated dispatch reaches
//! targets through the bound transport carrier, *not* through this client, so
//! the client here is an inert single-use handle that is never invoked — it
//! exists only to satisfy the trait bound and carry the shared-resource table.

use tokio::io::{DuplexStream, ReadHalf, WriteHalf};
use wasmtime::component::ResourceTable;
use wrpc_transport::frame::Oneshot;
use wrpc_wasmtime::{SharedResourceTable, WrpcCtx, WrpcCtxView};

/// The wRPC client handle type a guest store advertises to `wrpc-wasmtime`:
/// the in-process carrier's client, a single stream pair to one target's
/// server used for exactly one invocation.
pub type LinkClient = Oneshot<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

/// Per-store wRPC view state: an inert client plus the shared-resource table.
pub struct WrpcState {
    client: LinkClient,
    shared: SharedResourceTable,
}

impl WrpcState {
    /// Create fresh per-store wRPC view state.
    #[must_use]
    pub fn new() -> Self {
        // A dummy pipe whose server half is dropped immediately: this client is
        // never invoked (dispatch uses the carrier), so it never reads or writes.
        let (client, _server) = Oneshot::duplex(1);
        Self {
            client,
            shared: SharedResourceTable::default(),
        }
    }

    /// Borrow this state as a [`WrpcCtxView`] paired with the store's resource
    /// table — the shape `wrpc-wasmtime`'s [`wrpc_wasmtime::WrpcView`] returns.
    pub fn view<'a>(&'a mut self, table: &'a mut ResourceTable) -> WrpcCtxView<'a, LinkClient> {
        WrpcCtxView { ctx: self, table }
    }
}

impl Default for WrpcState {
    fn default() -> Self {
        Self::new()
    }
}

impl WrpcCtx<LinkClient> for WrpcState {
    fn context(&self) {}

    fn client(&self) -> &LinkClient {
        &self.client
    }

    fn shared_resources(&mut self) -> &mut SharedResourceTable {
        &mut self.shared
    }
}
