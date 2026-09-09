//! wRPC serve side for host-mediated exports.

use std::collections::HashMap;
use std::pin::pin;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use wasmtime::Store;
use wasmtime::component::types;
use wrpc_wasmtime::ServeExt as _;

use super::handle::DispatchHandle;
use super::transport::Endpoint;
use crate::chain::with_chain;
use crate::registry::Guest;
use crate::runtime::Runtime;
use crate::store::StoreCtx;

/// wRPC host-resource map shape (empty for the resource-free dynamic path).
type HostResources = HashMap<
    Box<str>,
    HashMap<Box<str>, (wasmtime::component::ResourceType, wasmtime::component::ResourceType)>,
>;

/// Builds a fresh, fully configured guest store per served invocation.
pub type StoreFactory<T> = Arc<dyn Fn() -> Store<T> + Send + Sync>;

/// Wire the serve side of every host-mediated interface.
///
/// Each target guest that exports a linked interface runs a wRPC server whose
/// handlers instantiate the guest *fresh per call* (instance-per-call). Every
/// registered guest is served (parked pending on the transport), then all of
/// them are published under one lifecycle transition so polyfilled imports can
/// reach them. `Deployment::assemble` calls this during bootstrap; only a
/// runtime assembled through [`Runtime::from_parts`](crate::Runtime::from_parts)
/// wires it explicitly.
///
/// Spawns one detached task per served function to drain its invocation stream.
///
/// # Errors
///
/// Returns an error if a guest's export cannot be served over the carrier, or
/// if a served guest already has an endpoint (`serve_links` ran twice).
pub async fn serve_links<B>(state: &Runtime<B>) -> Result<()>
where
    B: Clone + Send + Sync + 'static,
{
    let registry = state.registry();
    let handle = registry.dispatch();
    let transport = handle.transport();
    let factory = state.store_factory();
    let guests: Vec<_> = registry.guests().collect();

    // On any failure, release what is still parked so a failed bootstrap pins
    // nothing.
    let discard_from = |first_unpublished: usize| {
        for guest in &guests[first_unpublished..] {
            transport.discard(guest.id());
        }
    };

    for guest in &guests {
        if let Err(error) = serve_guest(Arc::clone(&factory), guest, handle).await {
            discard_from(0);
            return Err(error);
        }
    }

    let _lifecycle = registry.lifecycle_write();
    for (published, guest) in guests.iter().enumerate() {
        if let Err(error) = transport.publish(guest.id()) {
            discard_from(published + 1);
            return Err(error);
        }
    }
    Ok(())
}

/// Wire the serve side of one guest's host-mediated exports and park the
/// result as pending on the transport — with no endpoint when the guest
/// exports no linked interface, so a later call to it is diagnosed as
/// "registered but unlinked" rather than "not registered".
///
/// Shared by the bootstrap walk above and by dynamic registration
/// (serve-at-register); the registry's transactional publish then moves the
/// pending endpoint live together with the registry entry.
///
/// # Errors
///
/// Returns an error if a guest's export cannot be served over the carrier, or
/// the guest already has a pending endpoint.
pub async fn serve_guest<B>(
    factory: StoreFactory<StoreCtx<B>>, guest: &Guest<StoreCtx<B>>, handle: &DispatchHandle,
) -> Result<()>
where
    B: Clone + Send + Sync + 'static,
{
    let engine = guest.instance_pre().engine().clone();
    let component_ty = guest.component().component_type();
    // Built incrementally so an error part-way drops it and aborts the drains
    // already spawned.
    let mut endpoint: Option<Endpoint> = None;

    for (interface, types::ComponentExtern { ty, .. }) in component_ty.exports(&engine) {
        if !handle.links().contains(interface) {
            continue;
        }
        let types::ComponentItem::ComponentInstance(instance_ty) = ty else {
            continue;
        };
        for (func, types::ComponentExtern { ty, .. }) in instance_ty.exports(&engine) {
            let types::ComponentItem::ComponentFunc(func_ty) = ty else {
                continue;
            };
            let endpoint = endpoint.get_or_insert_with(Endpoint::new);
            let factory = Arc::clone(&factory);
            let stream = endpoint
                .server
                .serve_function(
                    move || factory(),
                    guest.instance_pre().clone(),
                    Arc::<HostResources>::default(),
                    func_ty,
                    interface,
                    func,
                )
                .await
                .with_context(|| {
                    format!("serving `{interface}/{func}` from guest `{}`", guest.id())
                })?;

            endpoint.drains.push(tokio::spawn(async move {
                let mut stream = pin!(stream);
                while let Some(invocation) = stream.next().await {
                    match invocation {
                        Ok((ctx, fut)) => {
                            // Re-establish the caller's chain context around
                            // the served invocation so nested dispatches it
                            // makes stay bounded per chain and inherit the
                            // chain's wall-clock policy.
                            tokio::spawn(with_chain(ctx, async move {
                                if let Err(error) = fut.await {
                                    tracing::error!(%error, "link serve invocation failed");
                                }
                            }));
                        }
                        Err(error) => tracing::error!(%error, "link serve accept failed"),
                    }
                }
            }));
        }
    }

    handle.transport().park(guest.id(), endpoint)
}
