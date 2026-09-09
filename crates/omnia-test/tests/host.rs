//! The component rung: real guest components from `crates/test-programs`
//! driven through `Deployment` over `Backends`.

use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context as _, Result, bail};
use omnia::wasmtime::component::Val;
use omnia::{ExitStatus, GuestId, Location, Runtime};
use omnia_test::host::{Backends, Deployment, ScriptedModel, scratch};
use omnia_test::{Exchange, SeenFormat};
use omnia_wasi_blobstore::WasiBlobstoreCtx as _;
use omnia_wasi_config::WasiConfig;
use omnia_wasi_keyvalue::WasiKeyValueCtx as _;
use omnia_wasi_model::WasiModel;
use omnia_wasi_otel::WasiOtel;

test_programs::foreach_config!();

// A production `runtime!` as an embedder would write it: the compiled-in
// deployment and its hosts, connected from the environment under `main`.
mod production {
    use omnia_wasi_keyvalue::{KeyValueDefault, WasiKeyValue};
    use omnia_wasi_model::{ModelDefault, WasiModel};
    use omnia_wasi_otel::{OtelDefault, WasiOtel};

    omnia::runtime!({
        mode: command,
        guests: [{ id: "echo", source: test_programs::MODEL_ECHO_TEXT }],
        hosts: {
            WasiModel: ModelDefault,
            WasiKeyValue: KeyValueDefault,
            WasiOtel: OtelDefault,
        },
    });
}

// The same shape with a plugin seam and a `.` path location the binary
// would serve from a directory that does not exist under test.
mod production_plugins {
    use omnia_wasi_otel::{OtelDefault, WasiOtel};

    omnia::runtime!({
        mode: command,
        link: { interfaces: ["omnia-test:link/ops"] },
        plugin: {
            locations: [{ name: ".", path: "/nonexistent/adapters" }],
        },
        guests: [{ id: "requester", source: test_programs::PLUGINS_LOAD_PATH }],
        hosts: { WasiOtel: OtelDefault },
    });
}

#[tokio::test]
async fn runtime_overlay_drives_the_production_wiring() {
    // The generated `main` and `run` stay untouched; the overlay reaches the
    // same `Hooks` through `run_with`.
    let _ = (production::main, production::run);
    let backends = Backends::defaults().await.model(ScriptedModel::answering(["second"]));
    let status = Deployment::from(production::manifest())
        .run_with::<production::Hooks, _>(backends.clone())
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS);

    let seen = backends.model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].messages, ["hi", "second"]);
    backends.model.assert_exhausted();
}

#[tokio::test]
async fn path_root_rewrites_the_production_location() {
    let _ = (production_plugins::main, production_plugins::run);
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");

    let deployment = Deployment::from(production_plugins::manifest())
        .mount(scratch.mount(false))
        .path_root(scratch.path());
    assert_eq!(
        deployment.manifest().expect("inline base resolves").plugin.locations,
        [Location::path(".", scratch.path())],
        "the overlay replaces the binary's `.` root rather than adding a second"
    );

    let status = deployment
        .run_with::<production_plugins::Hooks, _>(Backends::defaults().await)
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the requester's assertions all held");
}

#[tokio::test]
async fn scripted_model_answers_a_guest_completion() {
    let backends = Backends::defaults().await.model(ScriptedModel::answering(["second"]));
    let status = Deployment::new()
        .guest("echo", test_programs::MODEL_ECHO_TEXT)
        .run_host::<WasiModel, _>(backends.clone())
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS);

    let seen = backends.model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].system.as_deref(), Some("be terse"));
    assert_eq!(seen[0].messages, ["hi", "second"]);
    assert_eq!(seen[0].format, SeenFormat::Text);
    assert!(seen[0].workspace.is_none());
    backends.model.assert_exhausted();
}

#[tokio::test]
async fn scripted_calls_drive_the_guest_tool_handler() {
    let model = ScriptedModel::answering(["42"]).calling(0, [("lookup", "{}")]);
    let backends = Backends::defaults().await.model(model);
    let status = Deployment::new()
        .guest("tools", test_programs::MODEL_TOOL_ROUNDTRIP)
        .command("tools")
        .run_host::<WasiModel, _>(backends.clone())
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS);

    assert_eq!(backends.model.seen()[0].tools, ["lookup"]);
    assert_eq!(
        backends.model.exchanges(),
        [Exchange {
            tool: "lookup".into(),
            arguments: "{}".into(),
            outcome: Ok("42".into()),
        }]
    );
}

#[tokio::test]
async fn exhausted_script_fails_the_guest_not_the_test() {
    let backends = Backends::defaults().await.model(ScriptedModel::default());
    let outcome = Deployment::new()
        .guest("echo", test_programs::MODEL_ECHO_TEXT)
        .run_host::<WasiModel, _>(backends.clone())
        .await;
    assert!(!matches!(outcome, Ok(ExitStatus::SUCCESS)), "the guest's expect fails: {outcome:?}");
    assert_eq!(backends.model.seen().len(), 1, "the request was still recorded");
    // The soft answer to the guest is not a soft answer to the test: the
    // overrun fails the assertion the scenario would normally end with.
    let asserted = catch_unwind(AssertUnwindSafe(|| backends.model.assert_exhausted()));
    assert!(asserted.is_err(), "the overrun fails assert_exhausted()");
}

#[tokio::test]
async fn then_answers_past_the_script() {
    let model = ScriptedModel::answering::<String>([]).then(|| "second".to_owned());
    let backends = Backends::defaults().await.model(model);
    let status = Deployment::new()
        .guest("echo", test_programs::MODEL_ECHO_TEXT)
        .run_host::<WasiModel, _>(backends)
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS);
}

#[tokio::test]
async fn link_pair_dispatches_through_the_booted_runtime() {
    let runtime = Deployment::new()
        .link(["omnia-test:link/ops"])
        .guest("echoer", test_programs::LINK_ECHOER)
        .guest("full", test_programs::LINK_FULL)
        .boot(Backends::defaults().await, |_| Ok(()))
        .await
        .expect("deployment boots");

    let answer = call(&runtime, "full", "poke", "hi").await.expect("dispatch");
    assert_eq!(answer, "echoer pong: hi");
    runtime.shutdown();
}

#[tokio::test]
async fn path_root_serves_plugin_loads() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");

    let status = Deployment::new()
        .link(["omnia-test:link/ops"])
        .guest("requester", test_programs::PLUGINS_LOAD_PATH)
        .mount(scratch.mount(false))
        .path_root(scratch.path())
        .run(Backends::defaults().await, |deployment| {
            deployment.host::<WasiOtel, Backends>()?;
            Ok(())
        })
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the requester's assertions all held");
}

#[tokio::test]
async fn config_seeded() {
    let backends = Backends::defaults().await.config([("GREETING", "hello")]);
    let status = Deployment::new()
        .guest("config", test_programs::CONFIG_SEEDED)
        .run_host::<WasiConfig, _>(backends)
        .await
        .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the guest saw `GREETING` and not `PATH`");
}

#[tokio::test]
async fn readers_see_writes_through_the_host_handles() {
    let backends = Backends::defaults().await;
    let bucket = backends.keyvalue.open_bucket("cache".to_owned()).await.expect("bucket");
    bucket.set("k".to_owned(), b"v".to_vec()).await.expect("set");
    assert_eq!(backends.state("k").await, Some(b"v".to_vec()));
    assert_eq!(backends.state("missing").await, None);

    let container = backends.blobstore.create_container("c".to_owned()).await.expect("container");
    container.write_data("o".to_owned(), b"bytes".to_vec().into()).await.expect("write");
    assert_eq!(backends.object("c", "o").await, Some(b"bytes".to_vec()));
    assert_eq!(backends.object("absent", "o").await, None);
}

#[test]
fn scratch_writes_and_mounts() {
    let scratch = scratch();
    scratch.write("nested/file.txt", "hello");
    assert_eq!(scratch.read("nested/file.txt"), Some(b"hello".to_vec()));
    let mount = scratch.mount(true);
    assert_eq!((mount.name.as_str(), mount.writable), (".", true));
    assert_eq!(scratch.mount_as("project", false).name, "project");
    assert_eq!(mount.path, scratch.path());
}

/// Instantiate `guest` fresh and drive its exported `func` with one string
/// argument, returning the string result.
async fn call<B>(runtime: &Runtime<B>, guest: &str, func: &str, message: &str) -> Result<String>
where
    B: Clone + Send + Sync + 'static,
{
    let entry = runtime
        .registry()
        .get(&GuestId::from(guest))
        .with_context(|| format!("guest `{guest}` is not registered"))?;
    let mut store = runtime.build_store(runtime.store());
    let instance = runtime.instantiate(entry.instance_pre(), &mut store).await?;
    let export = instance
        .get_func(&mut store, func)
        .with_context(|| format!("guest `{guest}` exports `{func}`"))?;
    let mut results = vec![Val::Bool(false)];
    export
        .call_async(&mut store, &[Val::String(message.to_owned())], &mut results)
        .await
        .map_err(anyhow::Error::from)?;
    match results.into_iter().next() {
        Some(Val::String(answer)) => Ok(answer),
        other => bail!("`{guest}`'s `{func}` returned a non-string result: {other:?}"),
    }
}
