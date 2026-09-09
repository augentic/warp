//! Every typed refusal: registry location, malformed and mismatched pins, a
//! missing file, native (pre-compiled) bytes, a deployment-guest identity,
//! and a conflicting re-pin of an active package.

#![cfg(target_arch = "wasm32")]

wit_bindgen::generate!({
    world: "requester",
    path: "wit",
    generate_all,
});

use omnia::plugins::loader::{self, Error, Location, Plugin};

omnia_guest::command!(scenario);

async fn load(package: &str, path: &str, digest: Option<&str>) -> Result<Plugin, Error> {
    loader::load(package.to_owned(), Location::Path(path.to_owned()), digest.map(str::to_owned))
        .await
}

/// A `refused` discriminant whose description names the expected cause —
/// the message is what distinguishes the collapsed refusal scenarios.
fn assert_refused(err: &Error, cause: &str) {
    match err {
        Error::Refused(detail) => assert!(detail.contains(cause), "{detail}"),
        other => panic!("expected refused({cause}): {other:?}"),
    }
}

async fn scenario() {
    // The deployment's acquirer fills only the path slot; registry loads
    // refuse typed.
    let err = loader::load("test:reg".to_owned(), Location::Registry(None), None)
        .await
        .expect_err("registry locations are unsupported");
    assert_refused(&err, "serve no registry");

    // A malformed pin is refused before any acquisition.
    let err = load("test:badpin", "./plugin.wasm", Some("sha256:nothex"))
        .await
        .expect_err("malformed pin");
    assert_refused(&err, "hex characters");

    // A well-formed pin the bytes do not hash to.
    let wrong = format!("sha256:{}", "ab".repeat(32));
    let err =
        load("test:mismatch", "./plugin.wasm", Some(&wrong)).await.expect_err("mismatched pin");
    assert_refused(&err, "does not match the pinned");

    // A missing file is an acquisition failure, not a validation one.
    let err = load("test:absent", "./absent.wasm", None).await.expect_err("missing component");
    assert!(matches!(err, Error::Unavailable(_)), "{err:?}");

    // A native (pre-compiled) artifact is refused before wasmtime sees it.
    let err = load("test:native", "./native.bin", None).await.expect_err("native bytes");
    assert_refused(&err, "pre-compiled");

    // A deployment guest's identity can never be re-bound by a load.
    let err = load("requester", "./plugin.wasm", None).await.expect_err("static identity");
    assert!(matches!(err, Error::AlreadyActive(_)), "{err:?}");

    // An active loaded package refuses a conflicting re-pin.
    let plugin = load("test:echoer", "./plugin.wasm", None).await.expect("load succeeds");
    assert_eq!(plugin.id, "test:echoer");
    let err = load("test:echoer", "./plugin.wasm", Some(&wrong))
        .await
        .expect_err("conflicting pin for an active package");
    assert!(matches!(err, Error::AlreadyActive(_)), "{err:?}");
}
