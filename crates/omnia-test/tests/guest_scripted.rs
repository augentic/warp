//! The scripted pair at the handler rung: `Scripted` as a `Model`,
//! `ScriptedLoader` as a `Plugins` loader.

use std::panic::{AssertUnwindSafe, catch_unwind};

use omnia_guest::model::{
    Error, Format, Function, Message, Model, Request, Role, SchemaFormat, Tool, ToolCall,
};
use omnia_guest::plugins::{self, Digest, Location, PluginRef, Plugins};
use omnia_test::guest::{Scripted, ScriptedLoader, function_tools};
use omnia_test::{Exchange, SeenFormat};

fn user(content: &str) -> Request {
    Request::builder()
        .messages(vec![Message {
            role: Role::User,
            content: content.to_owned(),
        }])
        .build()
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn digest(fill: &str) -> Digest {
    format!("sha256:{}", fill.repeat(64 / fill.len())).parse().expect("digest")
}

#[tokio::test]
async fn answers_in_order() {
    let model = Scripted::answering(["first", "second"]);
    assert_eq!(model.complete(user("a")).await.expect("reply").answer, "first");
    assert_eq!(model.complete(user("b")).await.expect("reply").answer, "second");

    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages[0].content, "b");
    let seen = model.seen();
    assert_eq!(seen[0].messages, ["a"]);
    model.assert_exhausted();
}

#[tokio::test]
async fn scripted_failures() {
    let model = Scripted::new([Err(Error::BudgetExhausted("cap".into()))]);
    assert_eq!(model.complete(user("a")).await, Err(Error::BudgetExhausted("cap".into())));
}

#[tokio::test]
async fn complete_with_calls() {
    let model = Scripted::answering(["done"])
        .calling(0, [call("lookup", r#"{"id":1}"#), call("write", "{}")]);

    let reply = model
        .complete_with(user("go"), |call: ToolCall| async move {
            if call.name == "lookup" { Ok("found".to_owned()) } else { Err("denied".to_owned()) }
        })
        .await
        .expect("reply");

    assert_eq!(reply.answer, "done");
    assert_eq!(
        model.exchanges(),
        [
            Exchange {
                tool: "lookup".into(),
                arguments: r#"{"id":1}"#.into(),
                outcome: Ok("found".into()),
            },
            Exchange {
                tool: "write".into(),
                arguments: "{}".into(),
                outcome: Err("denied".into()),
            },
        ]
    );
}

fn checked(content: &str) -> Request {
    Request::builder()
        .messages(vec![Message {
            role: Role::User,
            content: content.to_owned(),
        }])
        .check(true)
        .build()
}

// The handler: accepts a candidate containing "ok", otherwise corrects.
async fn judge(call: ToolCall) -> Result<String, String> {
    assert_eq!(call.name, "check");
    if call.arguments.contains("ok") {
        Ok(String::new())
    } else {
        Err(format!("no: {}", call.arguments))
    }
}

#[tokio::test]
async fn check_accepts() {
    let model = Scripted::answering(["ok"]);
    let reply = model.complete_with(checked("go"), judge).await.expect("reply");
    assert_eq!(reply.answer, "ok");
    assert_eq!(
        model.exchanges(),
        [Exchange {
            tool: "check".into(),
            arguments: "ok".into(),
            outcome: Ok(String::new()),
        }]
    );
    model.assert_exhausted();
}

#[tokio::test]
async fn check_corrects() {
    let model = Scripted::answering(["bad", "ok"]);
    let reply = model.complete_with(checked("go"), judge).await.expect("reply");
    assert_eq!(reply.answer, "ok");
    let exchanges = model.exchanges();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0].outcome, Err("no: bad".into()));
    assert_eq!(exchanges[1].outcome, Ok(String::new()));
    assert_eq!(model.seen().len(), 2, "one scripted turn per attempt");
    model.assert_exhausted();
}

#[tokio::test]
async fn check_exhausts() {
    let model = Scripted::answering(["bad", "worse"]);
    let error = model.complete_with(checked("go"), judge).await.expect_err("never accepted");
    assert_eq!(error, Error::BudgetExhausted("no: worse".into()));
    model.assert_exhausted();
}

#[tokio::test]
async fn check_skips_failed() {
    let model = Scripted::new([Err(Error::Backend("offline".into()))]);
    let error = model.complete_with(checked("go"), judge).await.expect_err("backend failed");
    assert_eq!(error, Error::Backend("offline".into()));
    assert!(model.exchanges().is_empty(), "a failed turn has no candidate to check");
}

#[test]
fn complete_rejects_check() {
    let model = Scripted::default();
    let result = catch_unwind(AssertUnwindSafe(|| drop(model.complete(checked("a")))));
    assert!(result.is_err(), "complete must refuse a check request");
}

#[tokio::test]
async fn complete_rejects_calls() {
    let model = Scripted::answering(["x"]).calling(0, [call("t", "{}")]);
    let result = catch_unwind(AssertUnwindSafe(|| model.complete(user("a"))));
    assert!(result.is_err(), "complete must refuse scripted tool calls");
}

#[tokio::test]
async fn then_answers() {
    let model = Scripted::answering(["one"]).then(|| Err(Error::Backend("offline".into())));
    assert_eq!(model.complete(user("a")).await.expect("reply").answer, "one");
    assert_eq!(model.complete(user("b")).await, Err(Error::Backend("offline".into())));
}

#[test]
fn unscripted() {
    let model = Scripted::default();
    let result = catch_unwind(AssertUnwindSafe(|| drop(model.complete(user("a")))));
    assert!(result.is_err(), "an empty script panics on first use");
}

#[tokio::test]
async fn seen() {
    let model = Scripted::answering(["{}"]);
    let request = Request::builder()
        .system("be terse")
        .messages(vec![Message {
            role: Role::User,
            content: "hi".into(),
        }])
        .format(Format::Schema(SchemaFormat::builder().name("out").schema("{}").build()))
        .tools(vec![Tool::Function(
            Function::builder().name("lookup").description("d").parameters("{}").build(),
        )])
        .workspace(".")
        .build();
    model.complete(request).await.expect("reply");

    let seen = model.seen().remove(0);
    assert_eq!(seen.system.as_deref(), Some("be terse"));
    assert_eq!(seen.messages, ["hi"]);
    assert_eq!(
        seen.format,
        SeenFormat::Schema {
            name: "out".into(),
            schema: "{}".into()
        }
    );
    assert_eq!(seen.tools, ["lookup"]);
    assert_eq!(seen.workspace.as_deref(), Some("."));
    assert_eq!(function_tools(&model.requests()[0])[0].name, "lookup");
}

#[tokio::test]
async fn loader_digest() {
    let loader = ScriptedLoader::default().digest("acme:tool", digest("ab"));
    let plugin = loader
        .load(&PluginRef::builder().package("acme:tool").location(Location::Registry(None)).build())
        .await
        .expect("loads");
    assert_eq!(plugin.id(), "acme:tool");
    assert_eq!(*plugin.digest(), digest("ab"));
    assert_eq!(loader.loads().len(), 1);
}

#[tokio::test]
async fn loader_request_pin() {
    let loader = ScriptedLoader::default();
    let pinned = PluginRef::builder()
        .package("acme:tool")
        .location(Location::Path("./plugins".into()))
        .digest(digest("cd"))
        .build();
    let plugin = loader.load(&pinned).await.expect("loads");
    assert_eq!(*plugin.digest(), digest("cd"));

    let unpinned =
        PluginRef::builder().package("acme:other").location(Location::Registry(None)).build();
    let first = loader.load(&unpinned).await.expect("loads");
    let second = loader.load(&unpinned).await.expect("loads");
    assert_eq!(first.digest(), second.digest(), "placeholder digests are deterministic");
}

#[tokio::test]
async fn loader_disagreeing_pin() {
    let loader = ScriptedLoader::default().digest("acme:tool", digest("ab"));
    let pinned = PluginRef::builder()
        .package("acme:tool")
        .location(Location::Registry(None))
        .digest(digest("cd"))
        .build();
    match loader.load(&pinned).await {
        Err(plugins::Error::Refused(reason)) => {
            assert!(reason.contains("not the pinned"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// The default sits below the scripted digest and the request pin: an
// unscripted, unpinned load takes it in place of the placeholder; a pin of
// its own still wins; a pin disagreeing with a scripted digest is still
// refused rather than falling through to the default.
#[tokio::test]
async fn loader_defaulting() {
    let loader =
        ScriptedLoader::default().digest("acme:tool", digest("ab")).defaulting(digest("ef"));

    let unpinned =
        PluginRef::builder().package("acme:other").location(Location::Registry(None)).build();
    assert_eq!(*loader.load(&unpinned).await.expect("loads").digest(), digest("ef"));

    let pinned = PluginRef::builder()
        .package("acme:other")
        .location(Location::Path("./plugins".into()))
        .digest(digest("cd"))
        .build();
    assert_eq!(*loader.load(&pinned).await.expect("loads").digest(), digest("cd"));

    let disagreeing = PluginRef::builder()
        .package("acme:tool")
        .location(Location::Registry(None))
        .digest(digest("ef"))
        .build();
    assert!(matches!(loader.load(&disagreeing).await, Err(plugins::Error::Refused(_))));
    assert_eq!(loader.loads().len(), 3);
}

#[tokio::test]
async fn loader_scripted_refusal_wins() {
    let loader = ScriptedLoader::default()
        .digest("acme:tool", digest("ab"))
        .refuse("acme:tool", plugins::Error::Unavailable("registry down".into()));
    let unpinned =
        PluginRef::builder().package("acme:tool").location(Location::Registry(None)).build();
    assert_eq!(
        loader.load(&unpinned).await,
        Err(plugins::Error::Unavailable("registry down".into()))
    );
}
