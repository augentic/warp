//! Acquisition over wasm-pkg-client's `local` backend: fresh-release-preferred
//! resolution, the store as fallback and byte cache, poisoned entries,
//! endpoint overrides, and path locations — all offline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::FutureExt as _;
use futures::future::BoxFuture;
use omnia::sha256_digest;
use omnia_plugin::{
    ContentStore, PathMounts, PathSource as _, RegistryClient, RegistrySource as _, ReleaseStore,
};
use tempfile::TempDir;
use wasm_pkg_client::{Config, Registry};

const PACKAGE: &str = "test:adapter@1.0.0";
const DEFAULT_REGISTRY: &str = "registry.test";
// A closed local port: connection refused immediately, no network reached.
const UNROUTABLE_REGISTRY: &str = "127.0.0.1:1";

#[derive(serde::Serialize)]
struct LocalBackendConfig {
    root: PathBuf,
}

/// Stage `bytes` as `package` in a local-backend registry rooted at `root`.
fn stage(root: &Path, package: &str, bytes: &[u8]) {
    let (name, version) = package.split_once('@').expect("test packages pin versions");
    let (namespace, name) = name.split_once(':').expect("test packages are namespaced");
    let dir = root.join(namespace).join(name);
    std::fs::create_dir_all(&dir).expect("creating package directory");
    std::fs::write(dir.join(format!("{version}.wasm")), bytes).expect("staging package");
}

/// Register a `local`-backend registry named `name` in `config`.
fn add_local_registry(config: &mut Config, name: &str, root: &Path) {
    let registry: Registry = name.parse().expect("test registry name parses");
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
}

/// A cacheless acquirer whose default registry is a local backend at `root`.
fn registry_acquirer(root: &Path) -> RegistryClient {
    let mut config = Config::empty();
    add_local_registry(&mut config, DEFAULT_REGISTRY, root);
    RegistryClient::new(DEFAULT_REGISTRY).with_config(config)
}

type ReleaseKey = (String, String, String);

/// An in-memory [`ContentStore`] + [`ReleaseStore`] double: digest-keyed
/// content plus per-registry release records, with direct map access so
/// tests can inspect and poison entries without going through the traits.
#[derive(Clone, Default)]
struct MemStore {
    content: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    releases: Arc<Mutex<HashMap<ReleaseKey, String>>>,
}

impl MemStore {
    fn content_of(&self, digest: &str) -> Option<Vec<u8>> {
        self.content.lock().expect("content lock").get(digest).cloned()
    }

    fn poison(&self, digest: &str, bytes: &[u8]) {
        self.content.lock().expect("content lock").insert(digest.to_owned(), bytes.to_vec());
    }
}

impl ContentStore for MemStore {
    fn content<'a>(&'a self, digest: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Vec<u8>>>> {
        let bytes = self.content_of(digest);
        async move { Ok(bytes) }.boxed()
    }

    fn put_content<'a>(
        &'a self, digest: &'a str, bytes: &'a [u8],
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.content.lock().expect("content lock").insert(digest.to_owned(), bytes.to_vec());
        async move { Ok(()) }.boxed()
    }
}

impl ReleaseStore for MemStore {
    fn release<'a>(
        &'a self, registry: &'a str, package: &'a str, version: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
        let key = (registry.to_owned(), package.to_owned(), version.to_owned());
        let digest = self.releases.lock().expect("release lock").get(&key).cloned();
        async move { Ok(digest) }.boxed()
    }

    fn put_release<'a>(
        &'a self, registry: &'a str, package: &'a str, version: &'a str, digest: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        let key = (registry.to_owned(), package.to_owned(), version.to_owned());
        self.releases.lock().expect("release lock").insert(key, digest.to_owned());
        async move { Ok(()) }.boxed()
    }
}

#[tokio::test]
async fn registry_fetch() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"component bytes");
    let acquirer = registry_acquirer(registry.path()).cached(MemStore::default());

    let bytes = acquirer.acquire(PACKAGE, None).await.expect("acquires");
    assert_eq!(bytes, b"component bytes");
}

#[tokio::test]
async fn store_miss() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"component bytes");
    let store = MemStore::default();
    let acquirer = registry_acquirer(registry.path()).cached(store.clone());

    let digest = sha256_digest(b"component bytes");
    assert!(store.content_of(&digest).is_none(), "the store starts empty");
    acquirer.acquire(PACKAGE, None).await.expect("acquires");
    assert!(store.content_of(&digest).is_some(), "the store gains the digest-keyed entry");
}

#[tokio::test]
async fn fresh_over_warm() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"first bytes");
    let acquirer = registry_acquirer(registry.path()).cached(MemStore::default());
    acquirer.acquire(PACKAGE, None).await.expect("warms the store");

    // The registry re-publishes the same version with different content. A
    // release-record cache would keep serving the stored bytes; the fresh
    // resolution must win.
    stage(registry.path(), PACKAGE, b"second bytes");
    let bytes = acquirer.acquire(PACKAGE, None).await.expect("re-acquires");
    assert_eq!(bytes, b"second bytes", "the reachable registry is the authority");
}

#[tokio::test]
async fn network_failure_fallback() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"component bytes");
    let store = MemStore::default();

    // Warm the store under the unroutable registry *name*, served by a
    // local backend mapping.
    let mut config = Config::empty();
    add_local_registry(&mut config, UNROUTABLE_REGISTRY, registry.path());
    let warm = RegistryClient::new(UNROUTABLE_REGISTRY).with_config(config).cached(store.clone());
    warm.acquire(PACKAGE, None).await.expect("warms the store");

    // Same registry name and store, no backend mapping: resolution now dials
    // the closed port and fails as a network error, so the stored record and
    // content serve the load.
    let offline = RegistryClient::new(UNROUTABLE_REGISTRY).cached(store);
    let bytes = offline.acquire(PACKAGE, None).await.expect("falls back");
    assert_eq!(bytes, b"component bytes");
}

#[tokio::test]
async fn network_failure_no_record() {
    let acquirer = RegistryClient::new(UNROUTABLE_REGISTRY).cached(MemStore::default());

    let error = acquirer.acquire(PACKAGE, None).await.expect_err("nothing stored to fall back to");
    assert!(format!("{error:#}").contains("resolving"), "resolution failure: {error:?}");
}

#[tokio::test]
async fn poisoned_store() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"honest bytes");
    let store = MemStore::default();
    let acquirer = registry_acquirer(registry.path()).cached(store.clone());
    acquirer.acquire(PACKAGE, None).await.expect("warms the store");

    let digest = sha256_digest(b"honest bytes");
    store.poison(&digest, b"poison");

    let bytes = acquirer.acquire(PACKAGE, None).await.expect("a poisoned entry refetches");
    assert_eq!(bytes, b"honest bytes");
    let healed = store.content_of(&digest).expect("reading the store entry");
    assert_eq!(healed, b"honest bytes", "the refetch overwrites the poisoned entry");
}

#[tokio::test]
async fn release_scoped() {
    let default_root = TempDir::new().expect("default registry dir");
    stage(default_root.path(), PACKAGE, b"default registry bytes");
    let override_root = TempDir::new().expect("override registry dir");
    stage(override_root.path(), PACKAGE, b"override registry bytes");

    let mut config = Config::empty();
    add_local_registry(&mut config, DEFAULT_REGISTRY, default_root.path());
    add_local_registry(&mut config, "override.test", override_root.path());
    let acquirer =
        RegistryClient::new(DEFAULT_REGISTRY).with_config(config).cached(MemStore::default());

    let default_bytes = acquirer.acquire(PACKAGE, None).await.expect("default acquires");
    assert_eq!(default_bytes, b"default registry bytes");
    // Same package and version, same store: release records are scoped per
    // registry, so the override never answers from the default's record.
    let override_bytes =
        acquirer.acquire(PACKAGE, Some("override.test")).await.expect("override acquires");
    assert_eq!(override_bytes, b"override registry bytes");
}

#[tokio::test]
async fn cacheless() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"first bytes");
    let acquirer = registry_acquirer(registry.path());

    let first = acquirer.acquire(PACKAGE, None).await.expect("acquires");
    assert_eq!(first, b"first bytes");
    stage(registry.path(), PACKAGE, b"second bytes");
    let second = acquirer.acquire(PACKAGE, None).await.expect("re-acquires");
    assert_eq!(second, b"second bytes", "nothing cached anywhere");
}

#[tokio::test]
async fn unversioned_and_missing() {
    let registry = TempDir::new().expect("registry dir");
    stage(registry.path(), PACKAGE, b"component bytes");
    let acquirer = registry_acquirer(registry.path());

    let unversioned =
        acquirer.acquire("test:adapter", None).await.expect_err("exact version is mandatory");
    assert!(format!("{unversioned:#}").contains("exact version"), "refusal: {unversioned:?}");

    acquirer.acquire("test:absent@1.0.0", None).await.expect_err("an absent package fails");
}

#[tokio::test]
async fn path_locations() {
    let root = TempDir::new().expect("location dir");
    std::fs::write(root.path().join("plugin.wasm"), b"located bytes").expect("staging component");
    let acquirer = PathMounts::new([(".", root.path())]).expect("locations open at construction");

    let prefixed = acquirer.acquire("./plugin.wasm").await.expect("prefixed path reads");
    assert_eq!(prefixed, b"located bytes");
    let bare =
        acquirer.acquire("plugin.wasm").await.expect("bare path falls back to the `.` entry");
    assert_eq!(bare, b"located bytes");

    for path in ["./../secret.wasm", "/etc/passwd", ".\\x.wasm", "./a//b.wasm"] {
        acquirer.acquire(path).await.expect_err("escape refused");
    }
}

#[tokio::test]
async fn longest_location() {
    let outer = TempDir::new().expect("outer location");
    let inner = TempDir::new().expect("inner location");
    std::fs::write(inner.path().join("p.wasm"), b"inner").expect("staging component");
    std::fs::create_dir_all(outer.path().join("inner")).expect("creating decoy");
    std::fs::write(outer.path().join("inner").join("p.wasm"), b"outer").expect("staging decoy");
    let acquirer = PathMounts::new([("adapters", outer.path()), ("adapters/inner", inner.path())])
        .expect("locations open");

    let bytes = acquirer.acquire("adapters/inner/p.wasm").await.expect("longest prefix reads");
    assert_eq!(bytes, b"inner", "the more specific location serves the path");
}

#[tokio::test]
async fn unlocated_and_missing() {
    let root = TempDir::new().expect("location dir");
    let acquirer = PathMounts::new([("adapters", root.path())]).expect("location opens");

    acquirer.acquire("elsewhere/p.wasm").await.expect_err("no location matches");
    acquirer.acquire("adapters/absent.wasm").await.expect_err("file is absent");
}

#[tokio::test]
async fn path_fail_fast() {
    let error = PathMounts::new([("adapters", "/no/such/directory")])
        .expect_err("a missing location refuses at construction");
    assert!(format!("{error:#}").contains("adapters"), "the refusal names the location: {error}");
}
