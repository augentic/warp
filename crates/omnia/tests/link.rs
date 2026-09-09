//! End-to-end tests for host-mediated link dispatch: every scenario drives
//! real guest components from `crates/test-programs` through the omnia
//! runtime. `partial` imports a strict subset of the `omnia-test:link/ops`
//! functions `full` imports (the componentizer prunes unused imports), so the
//! suite proves the shared linker unions per-guest imports at function
//! granularity — at bootstrap and for late registration.

#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Context as _, Result, bail};
use omnia::wasmtime::component::Val;
use omnia::{DeploymentBuilder, GuestArtifact, GuestEntry, GuestId, Manifest, Runtime, StoreCtx};

// Every guest program in `crates/test-programs` must have a matching test
// here; a new program without one fails to compile.
test_programs::foreach_link!();

/// Boot a runtime over `guests` (assembled in order) with
/// `omnia-test:link/ops` dispatched.
async fn boot(guests: &[(&str, &str)]) -> Result<Runtime<()>> {
    let mut manifest = Manifest::new().link(["omnia-test:link/ops"]);
    for (id, wasm) in guests {
        manifest = manifest.guest(GuestEntry::new(*id, *wasm));
    }
    let deployment = DeploymentBuilder::new()
        .manifest(manifest)
        .build::<StoreCtx<()>>()
        .await
        .context("building deployment")?;
    deployment.assemble(()).await
}

/// Instantiate `guest` fresh and drive its exported `func` with one string
/// argument, returning the string result.
async fn call(runtime: &Runtime<()>, guest: &str, func: &str, message: &str) -> Result<String> {
    let entry = runtime
        .registry()
        .get(&GuestId::from(guest))
        .with_context(|| format!("guest `{guest}` is not registered"))?;
    let mut store = runtime.build_store(runtime.store());
    let instance = runtime
        .instantiate(entry.instance_pre(), &mut store)
        .await
        .with_context(|| format!("instantiating `{guest}`"))?;
    let export = instance
        .get_func(&mut store, func)
        .with_context(|| format!("guest `{guest}` exports `{func}`"))?;

    // `call_async` drives sync- and async-lifted exports alike.
    let mut results = vec![Val::Bool(false)];
    export
        .call_async(&mut store, &[Val::String(message.to_owned())], &mut results)
        .await
        .map_err(anyhow::Error::from)
        .with_context(|| format!("calling `{guest}`'s `{func}`"))?;
    match results.into_iter().next() {
        Some(Val::String(answer)) => Ok(answer),
        other => bail!("`{guest}`'s `{func}` returned a non-string result: {other:?}"),
    }
}

#[tokio::test]
async fn link_echoer() {
    let runtime =
        boot(&[("echoer", test_programs::LINK_ECHOER), ("full", test_programs::LINK_FULL)])
            .await
            .expect("deployment boots");

    // Both dispatch paths round-trip to the exporter.
    let sync = call(&runtime, "full", "poke", "hi").await.expect("sync dispatch");
    assert_eq!(sync, "echoer pong: hi");
    let concurrent = call(&runtime, "full", "poke-async", "hi").await.expect("async dispatch");
    assert_eq!(concurrent, "echoer pong-async: hi");
}

#[tokio::test]
async fn link_partial() {
    let runtime =
        boot(&[("echoer", test_programs::LINK_ECHOER), ("partial", test_programs::LINK_PARTIAL)])
            .await
            .expect("deployment boots");

    // An interface wired with only one of its functions still resolves.
    let answer = call(&runtime, "partial", "poke", "hi").await.expect("subset dispatch");
    assert_eq!(answer, "echoer pong: hi");
}

// The union regression: `partial` assembles first and wires only `ping`, so
// the linker must reopen the interface and add `full`'s `ping-async` rather
// than skip the already-seen interface (which failed `full`'s
// pre-instantiation before wiring went per-function).
#[tokio::test]
async fn link_full() {
    let runtime = boot(&[
        ("echoer", test_programs::LINK_ECHOER),
        ("partial", test_programs::LINK_PARTIAL),
        ("full", test_programs::LINK_FULL),
    ])
    .await
    .expect("subset-first deployment boots");

    let subset = call(&runtime, "partial", "poke", "one").await.expect("subset dispatch");
    assert_eq!(subset, "echoer pong: one");
    let sync = call(&runtime, "full", "poke", "two").await.expect("sync dispatch");
    assert_eq!(sync, "echoer pong: two");
    let concurrent = call(&runtime, "full", "poke-async", "three").await.expect("async dispatch");
    assert_eq!(concurrent, "echoer pong-async: three");
}

// The late dual of `link_full`: bootstrap wires only `partial`'s subset, so
// registering `full` afterwards must polyfill the missing `ping-async` on the
// linker clone.
#[tokio::test]
async fn link_full_registered_late() {
    let runtime =
        boot(&[("echoer", test_programs::LINK_ECHOER), ("partial", test_programs::LINK_PARTIAL)])
            .await
            .expect("deployment boots");

    let wasm = std::fs::read(test_programs::LINK_FULL).expect("reading full guest artifact");
    runtime.register("full", GuestArtifact::wasm(wasm)).await.expect("late registration");

    let sync = call(&runtime, "full", "poke", "late").await.expect("sync dispatch");
    assert_eq!(sync, "echoer pong: late");
    let concurrent = call(&runtime, "full", "poke-async", "late").await.expect("async dispatch");
    assert_eq!(concurrent, "echoer pong-async: late");
    // The bootstrap guest is untouched by the late wiring.
    let subset = call(&runtime, "partial", "poke", "still").await.expect("subset dispatch");
    assert_eq!(subset, "echoer pong: still");
}
