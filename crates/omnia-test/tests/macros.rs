//! `provider!` and `delegate!` expansions, plus the compile-fail diagnostics
//! under `tests/ui/`.

use std::sync::Arc;

use omnia_guest::model::{Message, Model, Request, Role};
use omnia_guest::{BlobStore, Config, Identity, Publish, StateStore};
use omnia_test::guest::{FixedIdentity, MapConfig, Memory, Scripted, Sink};

omnia_test::provider! {
    /// Every capability `provider!` knows.
    pub struct Everything: Config + HttpRequest + Identity + Publish + Broadcast + StateStore
        + BlobStore + DocumentStore + TableStore + Model + Plugins;
}

omnia_test::provider! {
    /// The exemplar's provider line.
    struct Exemplar: Config + DocumentStore + HttpRequest + Identity + Publish + StateStore
        + TableStore;
}

#[derive(Clone)]
struct Layered<S> {
    model: Scripted,
    storage: Arc<S>,
    identity: FixedIdentity,
}

omnia_test::delegate!(impl[S: StateStore + BlobStore + Send + Sync + 'static] Layered<S> {
    Model => model,
    StateStore + BlobStore => storage,
    Identity => identity,
});

fn user(content: &str) -> Request {
    Request::builder()
        .messages(vec![Message {
            role: Role::User,
            content: content.to_owned(),
        }])
        .build()
}

#[tokio::test]
async fn provider_seeds() {
    let provider = Everything::default()
        .config(MapConfig::default().with([("region", "eu")]))
        .identity(FixedIdentity::new("tok"))
        .model(Scripted::answering(["hi"]));
    provider.storage.insert_state("seen", b"1");

    assert_eq!(Config::get(&provider, "region").await.expect("config"), "eu");
    assert_eq!(provider.access_token("svc".into()).await.expect("token"), "tok");
    assert_eq!(StateStore::get(&provider, "seen").await.expect("state"), Some(b"1".to_vec()));
    BlobStore::put(&provider, "c", "o", b"blob").await.expect("blob");
    assert_eq!(
        provider.storage.object("c", "o"),
        Some(b"blob".to_vec()),
        "StateStore and BlobStore share the one storage double"
    );
    assert_eq!(provider.complete(user("q")).await.expect("reply").answer, "hi");
    Publish::send(&provider, "t", &omnia_guest::Message::new(b"m")).await.expect("publish");
    assert_eq!(provider.publish.sent().len(), 1);
    assert!(provider.broadcast.broadcasts().is_empty(), "Publish and Broadcast are separate sinks");
    provider.model.assert_exhausted();
}

#[tokio::test]
async fn exemplar_provider() {
    let provider = Exemplar::default();
    assert!(StateStore::get(&provider, "missing").await.expect("state").is_none());
    let _: &Sink = &provider.publish;
}

#[tokio::test]
async fn delegate_deref() {
    let storage = Arc::new(Memory::default());
    let provider = Layered {
        model: Scripted::answering(["ok"]),
        storage: Arc::clone(&storage),
        identity: FixedIdentity::new("t"),
    };
    provider.put("c", "o", b"bytes").await.expect("put");
    StateStore::set(&provider, "k", b"v", None).await.expect("set");

    assert_eq!(storage.object("c", "o"), Some(b"bytes".to_vec()));
    assert_eq!(storage.state("k"), Some(b"v".to_vec()));
    assert_eq!(provider.complete(user("q")).await.expect("reply").answer, "ok");
    assert_eq!(provider.identity.asked(), Vec::<String>::new());
}

#[test]
fn compile_fail_diagnostics() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
