//! wRPC serve side for host-mediated exports.

use std::collections::{BTreeSet, HashMap};
use std::pin::pin;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use wasmtime::component::{InstancePre, types};
use wasmtime_wasi::WasiView;
use wrpc_wasmtime::{ServeExt as _, WrpcView};

use super::transport::{Endpoint, InProcess};
use crate::chain::with_chain;
use crate::registry::GuestId;
use crate::seam::StoreFactory;

/// wRPC host-resource map shape (empty for the resource-free dynamic path).
type HostResources = HashMap<
    Box<str>,
    HashMap<Box<str>, (wasmtime::component::ResourceType, wasmtime::component::ResourceType)>,
>;

/// Wire the serve side of one guest's exports of the declared `interfaces`
/// and park the result as pending on the transport — with no endpoint when
/// the guest exports none of them, so a later call to it is diagnosed as
/// "registered but unlinked" rather than "not registered".
///
/// Each handler instantiates the guest *fresh per call* (instance-per-call)
/// on a store from `factory`. Spawns one detached task per served function to
/// drain its invocation stream; the registry's transactional publish then
/// moves the pending endpoint live together with the registry entry.
///
/// # Errors
///
/// Returns an error if an export cannot be served over the carrier, or the
/// guest already has a pending endpoint.
pub(super) async fn serve_guest<T>(
    transport: &InProcess, interfaces: &BTreeSet<Box<str>>, factory: StoreFactory<T>, id: &GuestId,
    instance_pre: InstancePre<T>,
) -> Result<()>
where
    T: WasiView + WrpcView + 'static,
{
    let engine = instance_pre.engine().clone();
    let component_ty = instance_pre.component().component_type();
    // Built incrementally so an error part-way drops it and aborts the drains
    // already spawned.
    let mut endpoint: Option<Endpoint> = None;

    for (interface, types::ComponentExtern { ty, .. }) in component_ty.exports(&engine) {
        if !interfaces.contains(interface) {
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
                    instance_pre.clone(),
                    Arc::<HostResources>::default(),
                    func_ty,
                    interface,
                    func,
                )
                .await
                .with_context(|| format!("serving `{interface}/{func}` from guest `{id}`"))?;

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

    transport.park(id, endpoint)
}
