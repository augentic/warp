//! One-shot `wasi:cli/run` command mode.

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use wasmtime_wasi::I32Exit;
use wasmtime_wasi::p3::bindings::{Command, CommandPre};

use super::{ExitStatus, Runtime};
use crate::registry::{Guest, GuestId, TriggerRouter};
use crate::store::StoreCtx;

/// Run the command guest once, after the [`Runtime`] is assembled.
///
/// A guest marked `command = true` in the manifest goes through the ordinary
/// registry lookup and fails the run if it is not registered. Without a
/// marked guest, the sole static `wasi:cli/run` exporter is the catch-all; a
/// deployment with no exporter is inert and exits `0`.
///
/// # Errors
///
/// Returns an error if the explicit command guest is not registered, routing
/// is ambiguous, the guest cannot be instantiated, or the command traps
/// without a guest exit code.
pub(super) async fn drive<B>(runtime: &Runtime<B>) -> Result<ExitStatus>
where
    B: Clone + Send + Sync + 'static,
{
    if let Some(id) = runtime.command_guest() {
        let id = id.clone();
        let guest = runtime
            .registry()
            .get(&id)
            .with_context(|| format!("command guest `{id}` is not registered"))?;
        return run_guest(runtime, &id, &guest).await;
    }

    let routing = TriggerRouter::build(
        runtime.registry(),
        "cli",
        runtime.registry().routes().cli().clone(),
        |pre| CommandPre::new(pre.clone()).map(|_| ()),
    )?;
    if routing.is_inert() {
        tracing::info!("no guest exports wasi:cli/run; cli trigger inert");
        return Ok(ExitStatus::SUCCESS);
    }
    let Some((guest_id, ())) = routing.catch_all() else {
        bail!("multiple wasi:cli/run guests; mark one `command = true` to disambiguate");
    };
    let guest = runtime
        .registry()
        .get(guest_id)
        .with_context(|| format!("routed guest `{guest_id}` is not registered"))?;
    run_guest(runtime, guest_id, &guest).await
}

/// Instantiate `guest` and drive its `wasi:cli/run` once.
async fn run_guest<B>(
    runtime: &Runtime<B>, guest_id: &GuestId, guest: &Arc<Guest<StoreCtx<B>>>,
) -> Result<ExitStatus>
where
    B: Clone + Send + Sync + 'static,
{
    tracing::debug!(guest = %guest_id, "running wasi:cli/run");

    let mut store = runtime.build_store(runtime.store());
    let instance = runtime.instantiate(guest.instance_pre(), &mut store).await?;
    let command = Command::new(&mut store, &instance)?;

    // A command chain root: link dispatches the guest makes (and their nested
    // hops) run without the `GUEST_TIMEOUT_MS` wall-clock cap, matching the
    // uncapped `wasi:cli/run` drive itself.
    let outcome = crate::chain::as_command_chain(
        store.run_concurrent(async move |store| command.wasi_cli_run().call_run(store).await),
    )
    .await;

    let status = match outcome {
        Ok(Ok(Ok(()))) => ExitStatus::SUCCESS,
        Ok(Ok(Err(()))) => ExitStatus::from(1),
        Ok(Err(error)) | Err(error) => match error.downcast_ref::<I32Exit>() {
            Some(exit) => ExitStatus::from(exit.0),
            None => return Err(error.into()),
        },
    };

    tracing::debug!(guest = %guest_id, code = status.code(), "wasi:cli/run exited");
    Ok(status)
}
