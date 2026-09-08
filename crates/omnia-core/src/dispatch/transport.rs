//! # Link transport seam
//!
//! Host-mediated calls ride [wRPC](https://github.com/bytecodealliance/wrpc) on
//! every leg; what is pluggable is the wRPC *transport*, not the RPC framework.
//! [`LinkTransport`] is that seam: the dispatch path only ever asks it to open a
//! client connection to a target, so "desktop -> cloud" becomes a transport
//! swap rather than a code change.
//!
//! Today it has one implementation, [`InProcess`]: full wRPC encode/decode over
//! an in-memory [`tokio::io::duplex`] byte pipe, with no network. Unix-domain
//! sockets, NATS and QUIC would slot in behind the same trait.
//!
//! The serve side is the registry itself: each target guest that exports a
//! host-mediated interface runs a wRPC [`Server`] whose handlers instantiate the
//! guest *fresh per call*. The carrier mints a fresh connection to that server
//! per invocation — closing the single-use limitation of a bare
//! [`Oneshot`](wrpc_transport::frame::Oneshot).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, PoisonError, RwLock};

use anyhow::{Context as _, Result, ensure};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf, split};
use wasmtime::component::ResourceTable;
use wrpc_transport::frame::{Oneshot, Server};
use wrpc_wasmtime::{SharedResourceTable, WrpcCtx, WrpcCtxView};

use crate::chain::ChainCtx;
use crate::registry::GuestId;

/// Default in-process pipe buffer size (64 kibibytes).
const DUPLEX_BUF: usize = 1 << 16;

/// The in-process wRPC server type: framed transport over a `tokio::io::duplex`
/// byte stream, one connection accepted per dispatched call. The accept
/// context carries the caller's chain context (depth plus wall-clock policy)
/// to the serve side.
pub type InProcServer = Server<ChainCtx, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

/// The in-process wRPC client handle: a single stream pair to one target's
/// server, used for exactly one invocation.
pub type InProcClient = Oneshot<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

/// The wRPC client handle type a guest store advertises to `wrpc-wasmtime`.
///
/// Re-exported for the `runtime!` macro's generated [`wrpc_wasmtime::WrpcView`]
/// implementation; equal to the in-process carrier's client.
pub type LinkClient = InProcClient;

/// A bound wRPC transport. The dispatch path talks only to this — never to a
/// concrete transport — so the same selector-driven dispatch runs co-located or
/// distributed.
///
/// Only [`InProcess`] implements it today; the trait is the seam a distributed
/// transport (UDS / NATS / QUIC) would extend.
pub trait LinkTransport: Send + Sync + 'static {
    /// The wRPC client handle this transport hands the dispatch path.
    type Client: wrpc_transport::Invoke<Context = ()>;

    /// Open a fresh client connection to `target` for a single invocation
    /// running at `ctx` in its dispatch chain; the transport carries the
    /// context to the serve side so nested calls stay bounded and inherit the
    /// chain's wall-clock policy.
    ///
    /// # Errors
    ///
    /// Returns an error if `target` has no bound endpoint on this transport.
    fn connect(&self, target: &GuestId, ctx: ChainCtx) -> Result<Self::Client>;
}

/// A guest's serve side: its wRPC server plus the detached tasks draining each
/// served function's invocation stream. Dropping the endpoint aborts the
/// drains, so removing a guest (or finishing the deployment) releases the
/// `Runtime` clones — and with them the engine — that the tasks pin.
pub struct Endpoint {
    server: Arc<InProcServer>,
    drains: Vec<tokio::task::JoinHandle<()>>,
}

impl Endpoint {
    /// Bundle a guest's wRPC server with its drain tasks.
    #[must_use]
    pub const fn new(server: Arc<InProcServer>, drains: Vec<tokio::task::JoinHandle<()>>) -> Self {
        Self { server, drains }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        for drain in &self.drains {
            drain.abort();
        }
    }
}

/// The co-located fast transport: every target's exports are served over a wRPC
/// [`Server`] reachable through an in-memory byte pipe.
///
/// The server map is `Arc`-shared behind interior mutability so a guest
/// registered after bootstrap gains an endpoint on every clone of the carrier
/// (serve-at-register). Mutations run with the registry's lifecycle write
/// guard already held by the caller (the transactional publish/remove), so the
/// registry map and this map change as one step; lookups take only the map's
/// own lock.
#[derive(Clone, Default)]
pub struct InProcess {
    servers: Arc<RwLock<HashMap<GuestId, Endpoint>>>,
}

impl InProcess {
    /// Returns the wRPC server serving `target`'s host-mediated exports, if any.
    #[must_use]
    pub fn server(&self, target: &GuestId) -> Option<Arc<InProcServer>> {
        self.servers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(target)
            .map(|endpoint| Arc::clone(&endpoint.server))
    }

    /// Add the endpoint serving `target`'s host-mediated exports, refusing an
    /// occupied slot so a registration can never clobber an existing guest's
    /// endpoint.
    ///
    /// The caller must hold the registry's lifecycle write guard; this method
    /// takes only the inner map lock.
    pub(crate) fn insert(&self, target: &GuestId, endpoint: Endpoint) -> Result<()> {
        let inserted = {
            let mut servers = self.servers.write().unwrap_or_else(PoisonError::into_inner);
            match servers.entry(target.clone()) {
                Entry::Occupied(_) => false,
                Entry::Vacant(slot) => {
                    slot.insert(endpoint);
                    true
                }
            }
        };
        ensure!(inserted, "guest `{target}` already has an in-process endpoint");
        Ok(())
    }

    /// Remove `target`'s endpoint, aborting its drain tasks; in-flight
    /// invocations hold their own server [`Arc`] and complete.
    ///
    /// The caller must hold the registry's lifecycle write guard.
    pub(crate) fn remove(&self, target: &GuestId) {
        self.servers.write().unwrap_or_else(PoisonError::into_inner).remove(target);
    }

    /// Drop every endpoint, aborting all drain tasks, so a finished deployment
    /// releases the `Runtime` clones (and the engine) they pin.
    pub(crate) fn clear(&self) {
        self.servers.write().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

/// Per-store wRPC view state.
///
/// `wrpc-wasmtime` requires each guest store to expose a [`WrpcCtx`] (a client
/// handle plus a shared-resource table). Omnia's host-mediated dispatch reaches
/// targets through the bound transport carrier, *not* through this client, so
/// the client here is an inert single-use handle that is never invoked — it
/// exists only to satisfy the trait bound and carry the shared-resource table.
pub struct WrpcState {
    client: InProcClient,
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
    pub fn view<'a>(&'a mut self, table: &'a mut ResourceTable) -> WrpcCtxView<'a, InProcClient> {
        WrpcCtxView { ctx: self, table }
    }
}

impl Default for WrpcState {
    fn default() -> Self {
        Self::new()
    }
}

impl WrpcCtx<InProcClient> for WrpcState {
    fn context(&self) {}

    fn client(&self) -> &InProcClient {
        &self.client
    }

    fn shared_resources(&mut self) -> &mut SharedResourceTable {
        &mut self.shared
    }
}

impl LinkTransport for InProcess {
    type Client = InProcClient;

    fn connect(&self, target: &GuestId, ctx: ChainCtx) -> Result<Self::Client> {
        let server = self.server(target).with_context(|| {
            format!(
                "no in-process endpoint serves guest `{target}` (is it registered and does it \
                     export the linked interface?)"
            )
        })?;

        // A fresh pipe per call: the client half drives this invocation; the
        // server half is accepted onto the target's wRPC server, which
        // instantiates the guest fresh (instance-per-call).
        let (client, server_stream) = Oneshot::duplex(DUPLEX_BUF);
        let (server_rx, server_tx) = split(server_stream);
        tokio::spawn(async move {
            if let Err(error) = server.accept(ctx, server_tx, server_rx).await {
                tracing::error!(%error, "in-process link accept failed");
            }
        });

        Ok(client)
    }
}
