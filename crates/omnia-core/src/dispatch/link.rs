//! Linker polyfill for host-mediated imports.

use std::collections::BTreeMap;
use std::iter::zip;
use std::pin::pin;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};
use bytes::BytesMut;
use tokio_util::codec::Encoder as _;
use wasmtime::component::{Accessor, Linker, Type, Val, types};
use wasmtime::{AsContextMut as _, Engine, StoreContextMut};
use wasmtime_wasi::WasiView;
use wrpc_transport::Invoke;
use wrpc_wasmtime::{ValEncoder, WrpcView, read_value};

use super::handle::DispatchHandle;
use super::transport::LinkTransport as _;
use super::value::read_plain_value;
use crate::artifact::LoadedGuest;
use crate::registry::GuestId;
use crate::value::contains_resource;

/// The functions polyfilled onto a linker — the union across guests at
/// function granularity, since components import only the functions they use
/// and so per-guest imports of one interface are arbitrary subsets. Keyed by
/// interface then function name; the value is the function's type-level
/// asyncness, so a later guest whose import disagrees is rejected instead of
/// failing wasmtime's pre-instantiation typecheck with no cross-guest context.
pub type WiredLinks = BTreeMap<Box<str>, BTreeMap<Box<str>, bool>>;

/// Polyfill every host-mediated import named in the deployment's declared
/// link interfaces onto the shared linker, bound to the dispatch handle,
/// returning the functions wired per interface.
///
/// Each function is linked exactly once (the linker is shared, so the
/// per-guest imports are unioned function-by-function, reopening an
/// interface's [`LinkerInstance`](wasmtime::component::LinkerInstance) as
/// later guests add functions). `wasi:*` imports are never touched here —
/// they are host-satisfied — so only the manifest-declared interfaces are
/// dispatched.
///
/// Runs *before* pre-instantiation, so an import that is neither host-satisfied
/// nor allow-listed remains unresolved and fails fast at `instantiate_pre`.
///
/// # Errors
///
/// Returns an error if a named link target is not an interface import, or if a
/// function cannot be defined on the linker.
pub fn link<T>(
    engine: &Engine, linker: &mut Linker<T>, guests: &[LoadedGuest], handle: &Arc<DispatchHandle>,
) -> Result<WiredLinks>
where
    T: WasiView + WrpcView + 'static,
{
    let mut wired = WiredLinks::new();
    if handle.links().is_empty() {
        return Ok(wired);
    }

    for LoadedGuest { id, component } in guests {
        polyfill_component(engine, linker, id, component, handle, &mut wired)?;
    }
    Ok(wired)
}

/// Polyfill a late (dynamically registered) component's allow-listed imports
/// onto `linker` — a clone of the shared linker, so the functions the
/// bootstrap already `wired` are skipped and the shared linker is never
/// mutated after assembly.
///
/// # Errors
///
/// Returns an error if a named link target is not an interface import, or if a
/// function cannot be defined on the linker.
pub fn polyfill_late<T>(
    engine: &Engine, linker: &mut Linker<T>, id: &GuestId,
    component: &wasmtime::component::Component, handle: &Arc<DispatchHandle>,
    bootstrap_wired: &WiredLinks,
) -> Result<()>
where
    T: WasiView + WrpcView + 'static,
{
    if handle.links().is_empty() {
        return Ok(());
    }
    let mut wired = bootstrap_wired.clone();
    polyfill_component(engine, linker, id, component, handle, &mut wired)
}

/// Polyfill one component's link-union imports not already in `wired`.
///
/// Registration matches the import's type-level asyncness: a plain `func` is
/// polyfilled with `func_new_async` ([`send`]), an `async func` with
/// `func_new_concurrent` ([`send_concurrent`]) — the sync-typed registration
/// would fail the pre-instantiation asyncness typecheck. A function an
/// earlier guest wired with the *other* asyncness is a cross-guest interface
/// disagreement, rejected here with both views named.
fn polyfill_component<T>(
    engine: &Engine, linker: &mut Linker<T>, id: &GuestId,
    component: &wasmtime::component::Component, handle: &Arc<DispatchHandle>,
    wired: &mut WiredLinks,
) -> Result<()>
where
    T: WasiView + WrpcView + 'static,
{
    let component_ty = component.component_type();
    for (name, types::ComponentExtern { ty, .. }) in component_ty.imports(engine) {
        if !handle.links().contains(name) {
            continue;
        }
        let types::ComponentItem::ComponentInstance(instance_ty) = ty else {
            bail!("link target `{name}` (imported by guest `{id}`) is not an interface");
        };

        // Snapshot the missing function names and asyncness before mutably
        // borrowing the linker, skipping functions an earlier guest wired.
        let wired_funcs = wired.entry(Box::from(name)).or_default();
        let describe = |is_async: bool| if is_async { "an async func" } else { "a plain func" };
        let mut funcs: Vec<(Arc<str>, bool)> = Vec::new();
        for (func, types::ComponentExtern { ty, .. }) in instance_ty.exports(engine) {
            let types::ComponentItem::ComponentFunc(ty) = ty else {
                continue;
            };
            let is_async = ty.async_();
            match wired_funcs.get(func) {
                Some(&earlier) if earlier == is_async => {}
                Some(&earlier) => bail!(
                    "guest `{id}` imports `{name}/{func}` as {}, but an earlier guest wired it \
                     as {}; every importer of a host-mediated function must agree on asyncness",
                    describe(is_async),
                    describe(earlier),
                ),
                None => funcs.push((Arc::from(func), is_async)),
            }
        }

        // Opening the instance also (re)defines it on the linker, so an
        // allow-listed interface resolves even when every function is already
        // wired (or it has none).
        let mut root = linker.root();
        let mut interface = root
            .instance(name)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("defining host-mediated interface `{name}`"))?;
        let iface_name: Arc<str> = Arc::from(name);

        for (func, is_async) in &funcs {
            let handle = Arc::clone(handle);
            let iface_name = Arc::clone(&iface_name);
            let func_name = Arc::clone(func);
            let registered = if *is_async {
                interface.func_new_concurrent(func, move |accessor, ty, params, results| {
                    let handle = Arc::clone(&handle);
                    let iface_name = Arc::clone(&iface_name);
                    let func_name = Arc::clone(&func_name);
                    Box::pin(async move {
                        send_concurrent(
                            accessor,
                            &handle,
                            &iface_name,
                            &func_name,
                            &ty,
                            params,
                            results,
                        )
                        .await
                        .map_err(wasmtime::Error::from_anyhow)
                    })
                })
            } else {
                interface.func_new_async(func, move |store, ty, params, results| {
                    let handle = Arc::clone(&handle);
                    let iface_name = Arc::clone(&iface_name);
                    let func_name = Arc::clone(&func_name);
                    Box::new(async move {
                        send(store, &handle, &iface_name, &func_name, &ty, params, results)
                            .await
                            .map_err(wasmtime::Error::from_anyhow)
                    })
                })
            };
            registered
                .map_err(anyhow::Error::from)
                .with_context(|| format!("polyfilling `{name}` function `{func}`"))?;
        }
        wired_funcs.extend(funcs.iter().map(|(func, is_async)| (Box::from(&**func), *is_async)));
    }
    Ok(())
}

/// A prepared dispatch: everything [`send`] and [`send_concurrent`] share
/// before they diverge on store threading.
struct Call<'a> {
    start: std::time::Instant,
    target: GuestId,
    forwarded: std::borrow::Cow<'a, [Val]>,
    param_types: Vec<Type>,
    result_types: Vec<Type>,
    client: super::transport::LinkClient,
    // Whether this call's chain root runs uncapped (command mode): the
    // round-trip then skips the `guest_timeout` wall-clock bound.
    uncapped: bool,
}

/// Shared per-call preamble: select the target, reject crossing resources,
/// take a depth slot, and open the client connection.
fn prepare<'a>(
    handle: &DispatchHandle, interface: &str, func: &str, ty: &types::ComponentFunc,
    params: &'a [Val],
) -> Result<Call<'a>> {
    let start = std::time::Instant::now();

    let (target, forwarded) = handle
        .selector
        .select(interface, func, params)
        .with_context(|| format!("selecting target for `{interface}/{func}`"))?;

    // Plain records cross by value; a live resource handle never crosses.
    for value in &*forwarded {
        if contains_resource(value) {
            bail!(
                "a resource handle cannot cross the link seam (call to `{interface}/{func}`, \
                 target `{target}`)"
            );
        }
    }

    let ctx = handle.policy.enter(&target)?;

    let param_types: Vec<Type> = ty.params().map(|(_, ty)| ty).collect();
    let result_types: Vec<Type> = ty.results().collect();
    ensure!(
        forwarded.len() == param_types.len(),
        "selector forwarded {} arguments but `{interface}/{func}` expects {}",
        forwarded.len(),
        param_types.len()
    );

    let client = handle.transport().connect(&target, interface, ctx)?;

    Ok(Call {
        start,
        target,
        forwarded,
        param_types,
        result_types,
        client,
        uncapped: ctx.uncapped,
    })
}

/// Encode the forwarded parameters with wRPC's value codec.
fn encode_params<T: WrpcView + 'static>(
    mut store: StoreContextMut<'_, T>, call: &Call<'_>, interface: &str, func: &str,
) -> Result<BytesMut> {
    let mut buf = BytesMut::new();
    for (value, ty) in zip(&*call.forwarded, &call.param_types) {
        let mut encoder = ValEncoder::new(store.as_context_mut(), ty, &[], &[]);
        encoder
            .encode(value, &mut buf)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("encoding parameter for `{interface}/{func}`"))?;
        ensure!(
            encoder.deferred.is_none(),
            "async/stream parameters cannot cross the link seam (`{interface}/{func}`)"
        );
    }
    Ok(buf)
}

fn timeout_error(
    handle: &DispatchHandle, target: &GuestId, interface: &str, func: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "link dispatch to `{target}` for `{interface}/{func}` timed out after {:?}",
        handle.policy.timeout
    )
}

/// Await the dispatch round-trip, bounded by `guest_timeout` unless the call's
/// chain root runs uncapped (a command-mode `wasi:cli/run` drive).
async fn bounded<F>(
    handle: &DispatchHandle, call: &Call<'_>, interface: &str, func: &str, fut: F,
) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    if call.uncapped {
        return fut.await;
    }
    tokio::time::timeout(handle.policy.timeout, fut)
        .await
        .map_err(|_elapsed| timeout_error(handle, &call.target, interface, func))?
}

fn log_dispatch(call: &Call<'_>, interface: &str, func: &str) {
    let elapsed_us = u64::try_from(call.start.elapsed().as_micros()).unwrap_or(u64::MAX);
    tracing::debug!(
        target = %call.target,
        interface,
        func,
        transport = "in-process",
        histogram.link_dispatch_duration_us = elapsed_us,
        monotonic_counter.link_dispatches = 1_u64,
        "dispatched host-mediated call",
    );
}

/// The per-call dispatch: select the target, reject crossing resources, bound
/// depth, then round-trip the call over the in-process wRPC carrier to a
/// freshly-instantiated target export.
async fn send<T>(
    mut store: StoreContextMut<'_, T>, handle: &DispatchHandle, interface: &str, func: &str,
    ty: &types::ComponentFunc, params: &[Val], results: &mut [Val],
) -> Result<()>
where
    T: WrpcView + 'static,
{
    let call = prepare(handle, interface, func, ty, params)?;
    let buf = encode_params(store.as_context_mut(), &call, interface, func)?;

    // Invoke over the carrier; the request is written and flushed here, the
    // results stream back on `incoming`. No deferred (async) parameters, so the
    // outgoing half carries nothing further and is dropped. On a server-rooted
    // chain the round-trip is bounded by `guest_timeout` so a hung target
    // cannot stall the caller; a command-rooted chain runs uncapped.
    let target = &call.target;
    let round_trip = async {
        let (_outgoing, incoming) =
            call.client.invoke((), interface, func, buf.freeze(), &[[]; 0]).await.with_context(
                || format!("invoking link target `{target}` for `{interface}/{func}`"),
            )?;

        let mut incoming = pin!(incoming);
        for (index, (value, ty)) in zip(results.iter_mut(), &call.result_types).enumerate() {
            read_value(&mut store, &mut incoming, &[], &[], value, ty, &[index])
                .await
                .map_err(anyhow::Error::from)
                .with_context(|| format!("decoding result {index} from `{target}`"))?;
        }
        anyhow::Ok(())
    };
    bounded(handle, &call, interface, func, round_trip).await?;

    log_dispatch(&call, interface, func);
    Ok(())
}

/// The concurrent dual of [`send`], for async-typed imports.
///
/// The store threading is the whole difference: a concurrent host task only
/// reaches the store synchronously via [`Accessor::with`], so parameters are
/// encoded inside a single `with` (the encoder never awaits) and results are
/// decoded store-free — sound because resources, the only values
/// `wrpc_wasmtime::read_value` needs the store for, never cross the link seam.
async fn send_concurrent<T>(
    accessor: &Accessor<T>, handle: &DispatchHandle, interface: &str, func: &str,
    ty: &types::ComponentFunc, params: &[Val], results: &mut [Val],
) -> Result<()>
where
    T: WrpcView + 'static,
{
    let call = prepare(handle, interface, func, ty, params)?;
    let buf = accessor
        .with(|mut access| encode_params(access.as_context_mut(), &call, interface, func))?;

    // Invoke over the carrier; see `send` for the streaming/timeout contract.
    let target = &call.target;
    let round_trip = async {
        let (_outgoing, incoming) =
            call.client.invoke((), interface, func, buf.freeze(), &[[]; 0]).await.with_context(
                || format!("invoking link target `{target}` for `{interface}/{func}`"),
            )?;

        let mut incoming = pin!(incoming);
        for (index, (value, ty)) in zip(results.iter_mut(), &call.result_types).enumerate() {
            read_plain_value(&mut incoming, value, ty)
                .await
                .with_context(|| format!("decoding result {index} from `{target}`"))?;
        }
        anyhow::Ok(())
    };
    bounded(handle, &call, interface, func, round_trip).await?;

    log_dispatch(&call, interface, func);
    Ok(())
}
