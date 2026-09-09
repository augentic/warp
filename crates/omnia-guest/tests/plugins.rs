//! `PluginCache` over a scripted loader: memoization by package identity.

#![cfg(not(target_arch = "wasm32"))]

use omnia_guest::Plugins;
use omnia_guest::plugins::{Digest, Error, Location, Plugin, PluginCache, PluginRef};
use omnia_test::guest::ScriptedLoader;

fn digest(hex_pair: &str) -> Digest {
    format!("sha256:{}", hex_pair.repeat(32)).parse().expect("a valid digest")
}

fn path_ref(package: &str, pin: Option<Digest>) -> PluginRef {
    PluginRef::builder()
        .package(package)
        .location(Location::Path("./plugin.wasm".into()))
        .maybe_digest(pin)
        .build()
}

#[tokio::test]
async fn ensure_once() {
    let loader = ScriptedLoader::default();
    let cache = PluginCache::new(loader.clone());
    let first = cache.ensure(&path_ref("test:echoer", None)).await.expect("cold load");
    let again = cache.ensure(&path_ref("test:echoer", None)).await.expect("memo hit");
    assert_eq!(first, again);
    assert_eq!(loader.loads().len(), 1);

    cache.ensure(&path_ref("test:other", None)).await.expect("distinct package loads");
    assert_eq!(loader.loads().len(), 2);
}

#[tokio::test]
async fn ensure_re_pin() {
    let loader = ScriptedLoader::default();
    let cache = PluginCache::new(loader.clone());
    cache.ensure(&path_ref("test:echoer", Some(digest("ab")))).await.expect("pinned load");

    let error = cache
        .ensure(&path_ref("test:echoer", Some(digest("cd"))))
        .await
        .expect_err("conflicting pin refused");
    assert!(matches!(error, Error::AlreadyActive(_)));
    assert_eq!(loader.loads().len(), 1, "the host is never re-asked");

    let matching = cache
        .ensure(&path_ref("test:echoer", Some(digest("ab"))))
        .await
        .expect("matching pin served from the memo");
    assert_eq!(matching.digest(), &digest("ab"));
}

// A cache drops into any `Plugins`-bounded caller and memoizes there.
#[tokio::test]
async fn cache_provider() {
    async fn load_twice<P: Plugins>(provider: &P) -> Plugin {
        let plugin = path_ref("test:echoer", None);
        Plugins::load(provider, &plugin).await.expect("cold load");
        Plugins::load(provider, &plugin).await.expect("memo hit")
    }

    let loader = ScriptedLoader::default();
    let held = load_twice(&PluginCache::new(loader.clone())).await;
    assert_eq!(loader.loads().len(), 1, "the memo answers the second load");
    assert_eq!(held.id(), "test:echoer");
}
