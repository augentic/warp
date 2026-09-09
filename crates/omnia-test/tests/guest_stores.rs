//! The store and trivial doubles at the handler rung.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::thread;

use bytes::Bytes;
use http::{Method, Request, Response};
use http_body_util::Full;
use omnia_guest::document_store::{Document, Filter, QueryOptions, SortField};
use omnia_guest::{
    BlobStore, BlobStoreExt, Broadcast, CasError, Config, DocumentStore, HttpRequest, Identity,
    Message, Publish, StateStore, TableStore,
};
use omnia_test::guest::{
    FixedIdentity, MapConfig, MatchedHttp, Memory, MemoryDocs, Namespaced, ScriptedTables, Sink,
};
use omnia_wasi_sql::{DataType, Field, Row};

/// Resolves a double's future; every store double answers without yielding.
fn now<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("a store double must not yield"),
    }
}

fn doc(id: &str, json: &str) -> Document {
    Document {
        id: id.to_owned(),
        data: json.as_bytes().to_vec(),
    }
}

// -- Memory: state -----------------------------------------------------------

#[test]
fn state_set_get_delete() {
    let memory = Memory::default();
    assert_eq!(now(StateStore::set(&memory, "k", b"v1", None)).expect("set"), None);
    assert_eq!(
        now(StateStore::set(&memory, "k", b"v2", Some(30))).expect("set"),
        Some(b"v1".to_vec())
    );
    assert_eq!(now(StateStore::get(&memory, "k")).expect("get"), Some(b"v2".to_vec()));
    assert_eq!(memory.state("k"), Some(b"v2".to_vec()));
    now(StateStore::delete(&memory, "k")).expect("delete");
    assert_eq!(now(StateStore::get(&memory, "k")).expect("get"), None);
    assert!(memory.is_empty());
}

#[test]
fn cas_conflict() {
    let memory = Memory::default();
    now(memory.cas("k", None, b"a")).expect("absent key swaps");
    assert_eq!(now(memory.cas("k", None, b"b")), Err(CasError::Conflict(Some(b"a".to_vec()))));
    now(memory.cas("k", Some(b"a"), b"b")).expect("matching value swaps");
    assert_eq!(memory.state("k"), Some(b"b".to_vec()));
}

#[test]
fn cas_race() {
    const WRITERS: usize = 8;
    const ROUNDS: u64 = 200;

    let memory = Memory::default();
    let handles: Vec<_> = (0..WRITERS)
        .map(|_| {
            let memory = memory.clone();
            thread::spawn(move || {
                for _ in 0..ROUNDS {
                    loop {
                        let current = memory.state("counter");
                        let value: u64 = current.as_deref().map_or(0, |bytes| {
                            std::str::from_utf8(bytes).unwrap().parse().unwrap()
                        });
                        let next = (value + 1).to_string();
                        if now(memory.cas("counter", current.as_deref(), next.as_bytes())).is_ok() {
                            break;
                        }
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer finishes");
    }

    let total = WRITERS as u64 * ROUNDS;
    assert_eq!(memory.state("counter"), Some(total.to_string().into_bytes()));
}

#[test]
fn increment() {
    let memory = Memory::default();
    assert_eq!(now(memory.increment("n", 5)).expect("increment"), 5);
    assert_eq!(now(memory.increment("n", -7)).expect("increment"), -2);
    assert_eq!(memory.state("n"), Some((-2i64).to_be_bytes().to_vec()));

    memory.insert_state("text", b"nope");
    let error = now(memory.increment("text", 1)).expect_err("non-integer bytes fail");
    assert!(format!("{error:#}").contains("8-byte"), "{error:#}");
}

// -- Memory: blobs -----------------------------------------------------------

#[test]
fn objects() {
    let memory = Memory::default();
    now(memory.create_container("c")).expect("create");
    assert!(now(memory.container_exists("c")).expect("exists"));
    now(memory.put("c", "b", b"bee")).expect("put");
    now(memory.put("c", "a", b"ay")).expect("put");
    assert_eq!(now(BlobStore::get(&memory, "c", "a")).expect("get"), Some(b"ay".to_vec()));
    assert!(now(memory.has("c", "b")).expect("has"));
    assert_eq!(now(memory.list("c")).expect("list"), ["a", "b"]);
    now(BlobStore::delete(&memory, "c", "a")).expect("delete");
    assert_eq!(memory.objects("c"), ["b"]);
    now(memory.delete_objects("c", &["b".to_owned()])).expect("delete many");
    assert!(memory.objects("c").is_empty());
    assert!(memory.has_container("c"));
    now(memory.delete_container("c")).expect("delete container");
    assert!(!memory.has_container("c"));
}

#[test]
fn get_range() {
    let memory = Memory::default();
    memory.insert_object("c", "o", b"0123456789");
    assert_eq!(now(memory.get_range("c", "o", 2, 4)).expect("range"), b"234");
    assert_eq!(now(memory.get_range("c", "o", 7, 0)).expect("unbounded"), b"789");
    assert_eq!(now(memory.get_range("c", "o", 8, u64::MAX)).expect("unbounded"), b"89");
    assert_eq!(now(memory.get_range("c", "o", 5, 100)).expect("clamped"), b"56789");
    assert!(now(memory.get_range("c", "o", 4, 2)).is_err(), "end before start fails");
    assert!(now(memory.get_range("c", "missing", 0, 0)).is_err(), "a missing object fails");
}

#[test]
fn info_timestamps() {
    let memory = Memory::default();
    now(memory.create_container("c")).expect("create");
    now(memory.put("c", "first", b"1")).expect("put");
    now(memory.put("c", "second", b"22")).expect("put");
    let container = now(memory.container_info("c")).expect("container info");
    let first = now(memory.object_info("c", "first")).expect("object info");
    let second = now(memory.object_info("c", "second")).expect("object info");
    assert!(container.created_at < first.created_at && first.created_at < second.created_at);
    assert_eq!((second.size, second.container.as_str()), (2, "c"));
}

#[test]
fn copy_move_and_clear() {
    let memory = Memory::default();
    memory.insert_object("src", "o", b"data");
    now(memory.copy_object("src", "o", "dst", "copy")).expect("copy");
    now(memory.move_object("src", "o", "dst", "moved")).expect("move");
    assert!(memory.objects("src").is_empty());
    assert_eq!(memory.objects("dst"), ["copy", "moved"]);
    now(memory.clear("dst")).expect("clear");
    assert!(memory.objects("dst").is_empty() && memory.has_container("dst"));
    assert!(now(memory.copy_object("src", "gone", "dst", "x")).is_err());
}

#[test]
fn namespaced() {
    let memory = Memory::default();
    let alpha = Namespaced::new("alpha", memory.clone());
    let beta = Namespaced::new("beta", memory.clone());
    now(StateStore::set(&alpha, "k", b"a", None)).expect("set");
    now(beta.put("c", "o", b"b")).expect("put");

    assert_eq!(now(StateStore::get(&beta, "k")).expect("get"), None);
    assert_eq!(memory.state("alpha/k"), Some(b"a".to_vec()));
    assert_eq!(memory.object("beta/c", "o"), Some(b"b".to_vec()));
    assert_eq!(now(beta.object_info("c", "o")).expect("info").container, "c");
    assert_eq!(now(beta.container_info("c")).expect("info").name, "c");
    let (state, blobs) = alpha.memory().snapshot();
    assert_eq!(state.len(), 1);
    assert_eq!(blobs.keys().collect::<Vec<_>>(), ["beta/c"]);
}

// -- Pointer impls -----------------------------------------------------------

/// A handler-shaped fn: bounded on the capability, not on `Memory`.
fn bump<P: StateStore>(store: &P) -> i64 {
    now(store.increment("visits", 1)).expect("increment")
}

#[test]
fn shared_handle() {
    let memory = Memory::default();
    let shared = Arc::new(memory.clone());
    assert_eq!(bump(&shared), 1);
    assert_eq!(bump(&&memory), 2);
    assert_eq!(bump(&Box::new(memory.clone())), 3);
    assert_eq!(memory.state("visits"), Some(3_i64.to_be_bytes().to_vec()));
    assert!(now(Arc::new(memory).has("c", "missing")).is_ok_and(|found| !found));
}

// -- MemoryDocs --------------------------------------------------------------

#[test]
fn documents() {
    let docs = MemoryDocs::default();
    now(docs.insert("people", &doc("1", r#"{"name":"ann"}"#))).expect("insert");
    assert!(now(docs.insert("people", &doc("1", "{}"))).is_err(), "insert is create-only");
    now(docs.put("people", &doc("1", r#"{"name":"anne"}"#))).expect("put upserts");
    let stored = now(DocumentStore::get(&docs, "people", "1")).expect("get").expect("present");
    assert_eq!(stored.data, br#"{"name":"anne"}"#);
    assert!(now(DocumentStore::delete(&docs, "people", "1")).expect("delete"));
    assert!(!now(DocumentStore::delete(&docs, "people", "1")).expect("delete"));
}

#[test]
fn queries() {
    let docs = MemoryDocs::default();
    for (id, age) in [("a", 30), ("b", 20), ("c", 40), ("d", 10)] {
        now(docs.put("people", &doc(id, &format!(r#"{{"age":{age}}}"#)))).expect("put");
    }

    let page = now(docs.query(
        "people",
        QueryOptions {
            filter: Some(Filter::gte("age", 20)),
            order_by: vec![SortField {
                field: "age".into(),
                descending: true,
            }],
            limit: Some(2),
            ..QueryOptions::default()
        },
    ))
    .expect("query");
    let ids: Vec<_> = page.documents.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["c", "a"]);
    assert!(page.continuation.is_some());

    let rest = now(docs.query(
        "people",
        QueryOptions {
            filter: Some(Filter::gte("age", 20)),
            order_by: vec![SortField {
                field: "age".into(),
                descending: true,
            }],
            limit: Some(2),
            continuation: page.continuation,
            ..QueryOptions::default()
        },
    ))
    .expect("query");
    let ids: Vec<_> = rest.documents.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["b"]);
    assert!(rest.continuation.is_none());
    assert_eq!(now(docs.documents("people")).expect("all").len(), 4);
}

// -- ScriptedTables ----------------------------------------------------------

#[test]
fn tables() {
    let row = Row {
        index: "0".into(),
        fields: vec![Field {
            name: "n".into(),
            value: DataType::Int32(Some(1)),
        }],
    };
    let tables = ScriptedTables::default()
        .on_query(|sql, _| sql.starts_with("SELECT"), vec![row])
        .on_exec(|sql, params| sql.starts_with("INSERT") && params.len() == 1, 1);

    let rows = now(tables.query("db".into(), "SELECT 1".into(), vec![])).expect("query");
    assert_eq!(rows.len(), 1);
    let affected =
        now(tables.exec("db".into(), "INSERT".into(), vec![DataType::Str(Some("x".into()))]))
            .expect("exec");
    assert_eq!(affected, 1);

    let statements = tables.statements();
    assert_eq!(statements.len(), 2);
    assert_eq!((statements[1].connection.as_str(), statements[1].sql.as_str()), ("db", "INSERT"));
}

#[test]
fn unmatched_statement() {
    let tables = ScriptedTables::default();
    let result = catch_unwind(AssertUnwindSafe(|| {
        drop(tables.exec("db".into(), "DROP TABLE t".into(), vec![]));
    }));
    let payload = result.expect_err("unmatched exec panics");
    let text = payload.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(text.contains("DROP TABLE t"), "{text}");
}

// -- MatchedHttp -------------------------------------------------------------

#[test]
fn http() {
    let http = MatchedHttp::default()
        .on(Method::GET, "https://api.test/ping", Response::new(Bytes::from_static(b"pong")))
        .on_matching(
            Method::POST,
            "https://api.test/items",
            |body| body.starts_with(b"{"),
            Response::builder().status(201).body(Bytes::new()).expect("response"),
        );

    let ping =
        Request::get("https://api.test/ping").body(Full::<Bytes>::default()).expect("request");
    assert_eq!(now(http.fetch(ping)).expect("fetch").into_body(), "pong");

    let create = Request::post("https://api.test/items")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(b"{\"a\":1}")))
        .expect("request");
    assert_eq!(now(http.fetch(create)).expect("fetch").status(), 201);

    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].text(), "{\"a\":1}");
    assert_eq!(requests[1].headers["content-type"], "application/json");
}

#[test]
fn unmatched_request() {
    let http = MatchedHttp::default();
    let request =
        Request::delete("https://api.test/x").body(Full::<Bytes>::default()).expect("request");
    let result = catch_unwind(AssertUnwindSafe(|| drop(now(http.fetch(request)))));
    let payload = result.expect_err("unmatched request panics");
    let text = payload.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(text.contains("DELETE https://api.test/x"), "{text}");
}

// -- Sink, MapConfig, FixedIdentity -----------------------------------------

#[test]
fn sink() {
    let sink = Sink::default();
    now(Publish::send(&sink, "orders", &Message::new(b"one"))).expect("send");
    now(Broadcast::send(&sink, "room", b"hello", Some(vec!["s1".into()]))).expect("broadcast");

    let sent = sink.sent();
    assert_eq!((sent[0].0.as_str(), sent[0].1.payload.as_slice()), ("orders", &b"one"[..]));
    let broadcasts = sink.broadcasts();
    assert_eq!(
        (broadcasts[0].name.as_str(), broadcasts[0].sockets.as_deref()),
        ("room", Some(&["s1".to_owned()][..]))
    );
}

#[test]
fn map_config() {
    let config = MapConfig::default().with([("region", "eu")]);
    assert_eq!(now(config.get("region")).expect("known"), "eu");
    let error = now(config.get("zone")).expect_err("unknown key");
    assert!(error.to_string().contains("zone"), "{error}");
}

#[test]
fn fixed_identity() {
    let identity = FixedIdentity::new("tok");
    assert_eq!(now(identity.access_token("svc".into())).expect("token"), "tok");
    assert_eq!(identity.asked(), ["svc"]);
}
