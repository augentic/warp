//! Host-originated dynamic dispatch into guest exports.

use anyhow::{Context as _, Result, bail};
use futures::FutureExt as _;
use wasmtime::component::{Val, types};

use crate::host::FutureResult;
use crate::registry::GuestId;
use crate::runtime::Runtime;
use crate::value::contains_resource;

/// Host-originated dynamic dispatch into a *known* guest export — the host→guest
/// counterpart of the selector-driven guest→guest `dispatch`.
///
/// Shares the depth bound (`ChainPolicy::enter`) and resource rejection with
/// guest→guest dispatch. The target is instantiated *fresh* on a new store and
/// the matching export invoked directly, so the callee can never re-enter its
/// caller and needs no declared link interface for `interface`.
///
/// `args` and the returned values are plain `Val`s; a live resource handle on
/// either side is rejected.
///
/// # Errors
///
/// Returns an error if the depth bound is exceeded, an argument or result carries
/// a resource handle, the target is not registered, the named `interface`/`func`
/// export is absent or is not a function, or the guest call traps.
pub async fn dispatch<B>(
    runtime: &Runtime<B>, target: &GuestId, interface: &str, func: &str, args: Vec<Val>,
) -> Result<Vec<Val>>
where
    B: Clone + Send + Sync + 'static,
{
    // Plain records cross by value; a live resource handle never crosses.
    for value in &args {
        if contains_resource(value) {
            bail!(
                "a resource handle cannot cross the link seam \
                 (host→guest `{interface}/{func}`, target `{target}`)"
            );
        }
    }

    let instance_pre = runtime
        .registry()
        .get(target)
        .with_context(|| {
            format!("dispatching `{interface}/{func}` to guest `{target}`: guest is not registered")
        })?
        .instance_pre()
        .clone();

    // Depth-count this hop exactly like a guest→guest dispatch. The guard
    // is held here (borrowing `runtime`) across the awaited callee task below.
    let _guard = runtime.registry().dispatch().policy.enter(target)?;

    // Run the callee on its own task. `resolve` is invoked from *within* the
    // caller guest's concurrent event loop (the backend's loop awaits it inside
    // the `complete` host call), and wasmtime forbids a recursive
    // `StoreContextMut::run_concurrent` on the same thread. Spawning gives the
    // callee a fresh event loop: when the caller's loop parks awaiting this task,
    // its ambient store clears, so the callee's call runs unnested. The task owns
    // the whole store lifecycle (build → instantiate → call → drop), so the
    // callee is a fresh instance that cannot re-enter its caller
    // (instance-per-call) and needs no declared link interface for `interface`.
    let task_runtime = (*runtime).clone();
    let target_owned = target.clone();
    let interface_owned = interface.to_owned();
    let func_owned = func.to_owned();
    let results = tokio::spawn(async move {
        let mut store = task_runtime.build_store(task_runtime.store());
        let instance = task_runtime
            .instantiate(&instance_pre, &mut store)
            .await
            .with_context(|| format!("instantiating dispatch target `{target_owned}`"))?;

        let interface_idx =
            instance.get_export_index(&mut store, None, &interface_owned).with_context(|| {
                format!("guest `{target_owned}` exports no interface `{interface_owned}`")
            })?;
        let (item, func_idx) = instance
            .get_export(&mut store, Some(&interface_idx), &func_owned)
            .with_context(|| {
            format!(
                "interface `{interface_owned}` (guest `{target_owned}`) exports no \
                     `{func_owned}`"
            )
        })?;
        let types::ComponentItem::ComponentFunc(func_ty) = item else {
            bail!("`{interface_owned}/{func_owned}` (guest `{target_owned}`) is not a function");
        };
        let result_count = func_ty.results().count();
        let function = instance.get_func(&mut store, func_idx).with_context(|| {
            format!("resolving `{interface_owned}/{func_owned}` on guest `{target_owned}`")
        })?;

        let mut results = vec![Val::Bool(false); result_count];
        function
            .call_async(&mut store, &args, &mut results)
            .await
            .map_err(anyhow::Error::from)
            .with_context(|| {
                format!("calling `{interface_owned}/{func_owned}` on guest `{target_owned}`")
            })?;
        Ok::<Vec<Val>, anyhow::Error>(results)
    })
    .await
    .with_context(|| format!("joining dispatch target `{target}` task"))?
    .with_context(|| format!("dispatching `{interface}/{func}` to guest `{target}`"))?;

    // A target must not hand back a resource handle either.
    for value in &results {
        if contains_resource(value) {
            bail!(
                "a resource handle cannot cross the link seam \
                 (result of `{interface}/{func}`, target `{target}`)"
            );
        }
    }

    Ok(results)
}

/// A host→guest call capability, type-erased so a host binding (e.g.
/// `wasi-model`'s `resolve`) can invoke a guest without naming the concrete
/// [`Runtime`].
///
/// The `runtime!` macro threads an `Arc<dyn Dispatcher>` into each store
/// context so any host binding gets dynamic host→guest calls for free. It
/// carries no consumer vocabulary — a consumer owns its own verb names and
/// return shapes and composes this generic seam.
pub trait Dispatcher: Send + Sync + 'static {
    /// Invoke `target`'s `interface`/`func` with `args`, returning the typed
    /// results. The target is instantiated *fresh* (instance-per-call), the hop
    /// is depth-bounded like any host-mediated call, and a live resource handle
    /// on either side is rejected.
    ///
    /// A `None` `interface` discovers the unique exported interface carrying a
    /// function named `func` — a structural component-model query that names no
    /// consumer scheme.
    fn invoke(
        &self, target: GuestId, interface: Option<String>, func: String, args: Vec<Val>,
    ) -> FutureResult<Vec<Val>>;
}

impl<B: Clone + Send + Sync + 'static> Dispatcher for crate::runtime::RuntimeDispatcher<B> {
    fn invoke(
        &self, target: GuestId, interface: Option<String>, func: String, args: Vec<Val>,
    ) -> FutureResult<Vec<Val>> {
        let runtime = self.runtime();
        async move {
            let interface: Box<str> = match interface {
                Some(name) => Box::from(name),
                None => find_interface(&runtime, &target, &func)?,
            };
            dispatch(&runtime, &target, &interface, &func, args).await
        }
        .boxed()
    }
}

/// Find the *unique* exported interface on `target`'s component that carries a
/// function named `func`, so a host can invoke it without hardcoding a
/// consumer interface name. Ambiguity is an error, never a silent first-match.
fn find_interface<B: Clone + Send + Sync + 'static>(
    runtime: &Runtime<B>, target: &GuestId, func: &str,
) -> Result<Box<str>> {
    let registry = runtime.registry();
    let engine = registry.engine();
    let guest = registry
        .get(target)
        .with_context(|| format!("dispatch target `{target}` is not registered"))?;
    let component_ty = guest.component().component_type();
    let mut matches: Vec<Box<str>> = Vec::new();
    for (interface, types::ComponentExtern { ty, .. }) in component_ty.exports(engine) {
        let types::ComponentItem::ComponentInstance(instance_ty) = ty else {
            continue;
        };
        let has_func =
            instance_ty.exports(engine).any(|(name, types::ComponentExtern { ty, .. })| {
                name == func && matches!(ty, types::ComponentItem::ComponentFunc(_))
            });
        if has_func {
            matches.push(Box::from(interface));
        }
    }
    match matches.len() {
        0 => bail!("dispatch target `{target}` exports no interface with a `{func}` function"),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "dispatch target `{target}` exports `{func}` from several interfaces ({}); name the \
             interface explicitly",
            matches.join(", ")
        ),
    }
}
