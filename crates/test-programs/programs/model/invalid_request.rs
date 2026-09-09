//! The host refuses malformed requests before the backend runs.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{
    Error, Format, Function, Model as _, Request, SchemaFormat, Tool, WasiModel,
};
use test_programs::user;

omnia_guest::command!(scenario);

async fn refused(request: Request) -> String {
    match WasiModel.complete(request).await {
        Err(Error::InvalidRequest(detail)) => detail,
        other => panic!("expected invalid-request, got {other:?}"),
    }
}

fn verdict(schema: &str) -> Format {
    Format::Schema(SchemaFormat::builder().name("verdict").schema(schema).build())
}

async fn scenario() {
    let empty = Request::builder().messages(vec![]).build();
    assert_eq!(refused(empty).await, "empty request");

    let blank = Request::builder().messages(vec![user("   ")]).build();
    assert_eq!(refused(blank).await, "empty request");

    let reserved = Request::builder()
        .messages(vec![user("hi")])
        .tools(vec![Tool::Function(
            Function::builder()
                .name("read")
                .description("shadow a host-injected tool")
                .parameters("{}")
                .build(),
        )])
        .build();
    assert!(refused(reserved).await.contains("reserved tool name"));

    let check = Request::builder()
        .messages(vec![user("hi")])
        .tools(vec![Tool::Function(
            Function::builder()
                .name("check")
                .description("shadow the check callback")
                .parameters("{}")
                .build(),
        )])
        .build();
    assert!(refused(check).await.contains("reserved tool name"));

    let bad_parameters = Request::builder()
        .messages(vec![user("hi")])
        .tools(vec![Tool::Function(
            Function::builder()
                .name("lookup")
                .description("look something up")
                .parameters("not json")
                .build(),
        )])
        .build();
    assert!(refused(bad_parameters).await.contains("`lookup`"));

    let unparseable_schema =
        Request::builder().messages(vec![user("hi")]).format(verdict("not json")).build();
    assert!(refused(unparseable_schema).await.contains("not valid JSON"));
}
