//! Without a `check`, a `format::schema` answer passes through as the
//! backend produced it: the schema steers the provider, nothing gates.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Format, Model as _, Request, SchemaFormat, WasiModel};
use test_programs::{VERDICT_SCHEMA, user};

omnia_guest::command!(scenario);

async fn scenario() {
    let request = Request::builder()
        .messages(vec![user("hi")])
        .format(Format::Schema(
            SchemaFormat::builder().name("verdict").schema(VERDICT_SCHEMA).build(),
        ))
        .build();

    let reply = WasiModel.complete(request).await.expect("the answer passes through");
    assert_eq!(reply.answer, r#"{"verdict":"pass"}"#);
}
