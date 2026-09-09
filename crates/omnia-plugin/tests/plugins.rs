//! End-to-end tests for the `omnia:plugins/loader` host capability: a real
//! requester guest from `crates/test-programs` drives loads through omnia's
//! runtime, with `PathMounts` reading components staged in a scratch mount or
//! a `RegistryClient` resolving them from a wasm-pkg `local` backend. The
//! requester asserts internally (handles, digests, dispatch answers, and
//! every typed refusal); the host side stages artifacts and checks the exit.
//! Lifecycle scenarios the WASI surface cannot reach (deregistration) drive
//! [`PluginLoader`] host-side over the same runtime.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use omnia::{
    DeploymentBuilder, ExitStatus, GuestArtifact, GuestEntry, LoadError, Location, Manifest, Mode,
    Origin, PathMounts, PathSource, PluginLoader as _, Plugins, RegistryClient, RegistrySource,
    Runtime, StoreCtx, WasiPlugins, sha256_digest,
};
use omnia_test::host::{Backends, Scratch, scratch};
use omnia_wasi_otel::WasiOtel;

// Every guest program in `crates/test-programs/programs/plugins` must have a
// matching test here; a new program without one fails to compile.
test_programs::foreach_plugins!();

/// Assemble the requester deployment around `wasm`: the scratch dir mounts at
/// `.`, `omnia-test:link/ops` is the declared plugin seam, and the two slots
/// are the compiled-in acquisition policy. The telemetry host serves the
/// `command!` guest's otel imports.
async fn requester_runtime(
    wasm: &str, scratch: &Scratch, registry: Option<Arc<dyn RegistrySource>>,
    path: Option<Arc<dyn PathSource>>,
) -> Result<Runtime<Backends>> {
    let manifest = Manifest::new()
        .link(["omnia-test:link/ops"])
        .guest(GuestEntry::new("requester", wasm))
        .mounts([scratch.mount(false)]);
    let mut deployment = DeploymentBuilder::new()
        .manifest(manifest)
        .mode(Mode::Command)
        .build::<StoreCtx<Backends>>()
        .await
        .context("building deployment")?;
    deployment.host::<WasiPlugins, Backends>()?;
    deployment.host::<WasiOtel, Backends>()?;
    let runtime =
        deployment.assemble(Backends::defaults().await).await.context("assembling runtime")?;
    Plugins::install(&runtime, registry, path)?;
    Ok(runtime)
}

/// Drive `wasm` as the `requester` command guest under the given slots.
async fn run_requester(
    wasm: &str, scratch: &Scratch, registry: Option<Arc<dyn RegistrySource>>,
    path: Option<Arc<dyn PathSource>>,
) -> Result<ExitStatus> {
    let runtime = requester_runtime(wasm, scratch, registry, path).await?;
    runtime.run_command().await
}

/// A `.`-rooted `PathMounts` over the scratch dir filling the path slot.
fn path_source(scratch: &Scratch) -> Arc<dyn PathSource> {
    Arc::new(PathMounts::new([(".", scratch.path())]).expect("opening the scratch location"))
}

#[derive(serde::Serialize)]
struct LocalBackendConfig {
    root: PathBuf,
}

/// Stage `wasm` as `package` in a wasm-pkg `local` backend rooted at `root`
/// and return a client whose default registry `registry.test` serves it.
fn registry_source(root: &Path, package: &str, wasm: &str) -> Arc<dyn RegistrySource> {
    let (name, version) = package.split_once('@').expect("test packages pin versions");
    let (namespace, name) = name.split_once(':').expect("test packages are namespaced");
    let dir = root.join(namespace).join(name);
    std::fs::create_dir_all(&dir).expect("creating package directory");
    std::fs::copy(wasm, dir.join(format!("{version}.wasm"))).expect("staging package");

    let registry: wasm_pkg_client::Registry =
        "registry.test".parse().expect("test registry name parses");
    let mut config = wasm_pkg_client::Config::empty();
    let backend = config.get_or_insert_registry_config_mut(&registry);
    backend.set_default_backend(Some("local".into()));
    backend
        .set_backend_config(
            "local",
            LocalBackendConfig {
                root: root.to_path_buf(),
            },
        )
        .expect("local backend config serializes");
    Arc::new(RegistryClient::new("registry.test").with_config(config))
}

#[tokio::test]
async fn plugins_load_path() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");

    let status = run_requester(
        test_programs::PLUGINS_LOAD_PATH,
        &scratch,
        None,
        Some(path_source(&scratch)),
    )
    .await
    .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the requester's assertions all held");
}

#[tokio::test]
async fn plugins_load_registry() {
    let scratch = scratch();
    let registry = registry_source(scratch.path(), "test:echoer@1.0.0", test_programs::LINK_ECHOER);

    let status =
        run_requester(test_programs::PLUGINS_LOAD_REGISTRY, &scratch, Some(registry), None)
            .await
            .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the requester's assertions all held");
}

// The guest-side copies exist because published crates cannot reference WIT
// outside their package root; the host copy stays canonical.
#[test]
fn wit_copies_stay_identical() {
    let canonical = include_str!("../wit/plugins.wit");
    assert_eq!(
        include_str!("../../omnia-guest/wit/plugins.wit"),
        canonical,
        "omnia-guest's plugins.wit copy drifted from crates/omnia-plugin/wit/plugins.wit"
    );
    assert_eq!(
        include_str!("../../test-programs/wit/deps/plugins/plugins.wit"),
        canonical,
        "test-programs' plugins.wit copy drifted from crates/omnia-plugin/wit/plugins.wit"
    );
}

// Compile-time proof that the macro's `locations:` grammar lowers into
// manifest data this crate's `Plugins::install_declared` consumes: path
// entries fold into a `PathMounts`, the registry entry into a
// `RegistryClient`, each filling its slot on `Plugins`. The macro's snapshot
// suite pins the expansion shape; this pins the types and the carried data.
mod locations_grammar {
    omnia::runtime!({
        link: { interfaces: ["omnia-test:link/ops"] },
        plugin: {
            locations: [
                { name: "adapters", path: "adapters" },
                { registry: "ghcr.io" },
            ],
        },
        guests: [
            { id: "engine", source: "engine.wasm" },
        ],
    });
}

#[test]
fn locations_grammar_carries_manifest_data() {
    // Touch the generated entry points so the compile-only module above is
    // reachable for dead-code analysis.
    let _ = (locations_grammar::main, locations_grammar::run, locations_grammar::run_with::<()>);
    let manifest = locations_grammar::manifest().into_manifest().expect("inline source resolves");
    assert_eq!(
        manifest.plugin.locations,
        [Location::path("adapters", "adapters"), Location::registry("ghcr.io"),]
    );
}

#[tokio::test]
async fn plugins_load_refused() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");
    // Leading ELF magic is exactly what the loader sniffs; the tail is junk,
    // proving refusal happens before any wasmtime parsing.
    std::fs::write(scratch.path().join("native.bin"), [0x7f, b'E', b'L', b'F', 0, 0, 0, 0])
        .expect("staging native bytes");

    let status = run_requester(
        test_programs::PLUGINS_LOAD_REFUSED,
        &scratch,
        None,
        Some(path_source(&scratch)),
    )
    .await
    .expect("deployment runs");
    assert_eq!(status, ExitStatus::SUCCESS, "the requester's assertions all held");
}

// Admission no longer requires a linked export: a component exporting no
// `omnia-test:link/ops` loads (it stays reachable through the host
// `Dispatcher`), and a link call to it fails at the call site — the polyfill
// traps the requester, so the failure is observed here, after the load.
#[tokio::test]
async fn plugins_load_unlinked() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_FULL, scratch.path().join("noseam.wasm"))
        .expect("staging the unlinked component");

    let runtime = requester_runtime(
        test_programs::PLUGINS_LOAD_UNLINKED,
        &scratch,
        None,
        Some(path_source(&scratch)),
    )
    .await
    .expect("assembling runtime");

    let error = runtime.run_command().await.expect_err("the link call traps the requester");
    assert!(
        runtime.registry().get(&"test:unlinked".into()).is_some(),
        "the load succeeded before the call failed: {error:#}"
    );
    let detail = format!("{error:#}");
    assert!(
        detail.contains("guest `test:unlinked` is registered but exports no linked interface"),
        "the trap diagnoses the unlinked target: {detail}"
    );
    runtime.shutdown();
}

/// A wasm custom section (id 0) named `omnia-test` wrapping `payload`:
/// appending one changes a component's bytes — and digest — without changing
/// its behavior. Single-byte LEB128 sizes, so name plus payload stay short.
fn custom_section(payload: &[u8]) -> Vec<u8> {
    let name = b"omnia-test";
    let mut body = vec![u8::try_from(name.len()).expect("short name")];
    body.extend_from_slice(name);
    body.extend_from_slice(payload);
    let mut section = vec![0x00, u8::try_from(body.len()).expect("short section")];
    section.extend_from_slice(&body);
    section
}

// Host-side loads over the same runtime the guests drive: deregistration is
// host authority, so the WASI surface cannot reach this scenario. A
// deregistered package's digest record must not survive into the next load —
// the re-load binds the freshly staged bytes, and a stale pin refuses.
#[tokio::test]
async fn reload_after_deregister_binds_fresh_bytes() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");
    // The requester guest is manifest ballast: these loads are host-driven.
    let runtime = requester_runtime(
        test_programs::PLUGINS_LOAD_PATH,
        &scratch,
        None,
        Some(path_source(&scratch)),
    )
    .await
    .expect("assembling runtime");
    let origin = || Origin::Path("./plugin.wasm".to_owned());

    let first = runtime.load("test:echoer", origin(), None).await.expect("first load");
    runtime.deregister(first.id()).expect("deregistering the loaded plugin");

    // Same component, one extra custom section: same behavior, new digest.
    let mut changed = std::fs::read(test_programs::LINK_ECHOER).expect("reading the echoer");
    changed.extend_from_slice(&custom_section(b"reload"));
    std::fs::write(scratch.path().join("plugin.wasm"), &changed).expect("re-staging");

    let stale = runtime
        .load("test:echoer", origin(), Some(first.digest()))
        .await
        .expect_err("the old digest no longer matches the staged bytes");
    match &stale {
        LoadError::Refused(detail) => {
            assert!(detail.contains("does not match the pinned"), "{detail}");
        }
        other => panic!("expected a digest-mismatch refusal: {other:?}"),
    }

    let fresh = runtime.load("test:echoer", origin(), None).await.expect("re-load");
    assert_ne!(fresh.digest(), first.digest(), "the re-load bound fresh bytes");
    assert_eq!(fresh.digest(), sha256_digest(&changed));
    runtime.shutdown();
}

// The digest record lives on the registry entry, so an embedder swapping the
// identity outside the load path (deregister + `Runtime::register`) leaves no
// stale attestation behind: a pinned re-load must refuse rather than answer
// with the old digest over the new bytes.
#[tokio::test]
async fn pinned_reload_refuses_after_external_reregistration() {
    let scratch = scratch();
    std::fs::copy(test_programs::LINK_ECHOER, scratch.path().join("plugin.wasm"))
        .expect("staging the loadable echoer");
    let runtime = requester_runtime(
        test_programs::PLUGINS_LOAD_PATH,
        &scratch,
        None,
        Some(path_source(&scratch)),
    )
    .await
    .expect("assembling runtime");
    let origin = || Origin::Path("./plugin.wasm".to_owned());

    let first = runtime.load("test:echoer", origin(), None).await.expect("first load");
    runtime.deregister(first.id()).expect("deregistering the loaded plugin");

    // Same component, one extra custom section: same behavior, new digest —
    // registered by the embedder, not through the loader.
    let mut changed = std::fs::read(test_programs::LINK_ECHOER).expect("reading the echoer");
    changed.extend_from_slice(&custom_section(b"reregister"));
    runtime
        .register("test:echoer", GuestArtifact::wasm(changed))
        .await
        .expect("re-registering externally");

    let stale = runtime
        .load("test:echoer", origin(), Some(first.digest()))
        .await
        .expect_err("a load never attests an externally registered guest");
    assert!(matches!(stale, LoadError::AlreadyActive(_)), "{stale:?}");
    runtime.shutdown();
}

// A deployment that links the loader host but never installs the `Plugins`
// extension refuses every load as loader misconfiguration.
#[tokio::test]
async fn load_without_plugins_installed_refuses() {
    let scratch = scratch();
    let manifest = Manifest::new()
        .link(["omnia-test:link/ops"])
        .guest(GuestEntry::new("requester", test_programs::PLUGINS_LOAD_PATH))
        .mounts([scratch.mount(false)]);
    let mut deployment = DeploymentBuilder::new()
        .manifest(manifest)
        .mode(Mode::Command)
        .build::<StoreCtx<Backends>>()
        .await
        .expect("building deployment");
    deployment.host::<WasiPlugins, Backends>().expect("linking the plugins host");
    deployment.host::<WasiOtel, Backends>().expect("linking the otel host");
    let runtime =
        deployment.assemble(Backends::defaults().await).await.expect("assembling runtime");

    let error = runtime
        .load("test:echoer", Origin::Path("./plugin.wasm".to_owned()), None)
        .await
        .expect_err("a deployment without plugins refuses every load");
    assert!(
        matches!(&error, LoadError::Internal(detail) if detail.contains("has no plugins")),
        "{error:?}"
    );
    runtime.shutdown();
}
