//! Command façade contracts: argv parsing, the failure envelope, completions,
//! the projector, and the exit boundary.

#![cfg(all(not(target_arch = "wasm32"), feature = "command"))]

use std::borrow::Cow;
use std::fmt;
use std::future::{Ready, ready};

use clap::{Parser, Subcommand};
use omnia_guest::api::command::{
    Command, Failure, IntoExit, Parsed, Response, Shell, USAGE_EXIT, completions, parse,
};
use omnia_guest::api::{Client, Context, Format, Metadata};
use omnia_guest::{bad_request, not_found};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "app", bin_name = "app", version = "1.2.3", subcommand_required = true)]
struct App {
    #[arg(long, default_value = "text", global = true)]
    format: Format,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    Greet { name: String },
    Completions { shell: Shell },
}

#[test]
fn parse_app() {
    let Parsed::App(app) = parse::<App>(["app", "--format", "json", "greet", "ada"]) else {
        panic!("argv parses into the grammar");
    };

    assert_eq!(app.format, Format::Json);
    assert!(matches!(app.verb, Verb::Greet { name } if name == "ada"));
}

#[test]
fn help_is_display_not_usage() {
    let Parsed::Display(text) = parse::<App>(["app", "--help"]) else {
        panic!("help is a display outcome");
    };

    assert!(text.contains("Usage: app"));
    assert!(text.contains("greet"));
}

#[test]
fn version_is_display() {
    let Parsed::Display(text) = parse::<App>(["app", "--version"]) else {
        panic!("version is a display outcome");
    };

    assert_eq!(text.trim(), "app 1.2.3");
}

#[test]
fn usage_error_exits_64() {
    let Parsed::Usage(error) = parse::<App>(["app", "bogus"]) else {
        panic!("an unknown verb is a usage error");
    };
    let response = Response::usage(&error);

    assert_eq!(response.exit, USAGE_EXIT);
    assert_eq!(response.exit, 64);
    assert!(response.stdout.is_empty());
    let stderr = String::from_utf8(response.stderr).expect("clap renders utf-8");
    assert!(stderr.contains("bogus"));
    assert!(stderr.contains("Usage: app"));
}

#[test]
fn failure_json_is_flat_kebab_envelope() {
    let plain = Failure::from(not_found!("no item 42"));
    assert_eq!(plain.exit_code(), 2);
    assert_eq!(plain.hint(), None);
    assert_eq!(
        serde_json::to_value(&plain).expect("envelope serializes"),
        serde_json::json!({ "error": "not_found", "message": "no item 42", "exit-code": 2 })
    );

    let hinted = Failure::from(bad_request!("name is required")).with_hint("pass --name");
    assert_eq!(hinted.body().error, "bad_request");
    assert_eq!(
        serde_json::to_value(&hinted).expect("envelope serializes"),
        serde_json::json!({
            "error": "bad_request",
            "message": "name is required",
            "exit-code": 1,
            "hint": "pass --name"
        })
    );
}

#[test]
fn failure_text_with_hint() {
    let failure = Failure::from(not_found!("no item 42")).with_hint("run `app list`");

    let mut text = String::new();
    failure.text(&mut text).expect("a String sink never fails");
    assert_eq!(text, "error[not_found]: no item 42\nhint: run `app list`\n");

    let encoded = Format::Text.encode(&failure, Failure::text);
    assert_eq!(encoded.bytes, text.into_bytes());
}

#[test]
fn failure_from_anyhow_classifies_through_error() {
    let foreign = Failure::from(anyhow::anyhow!("disk on fire"));
    assert_eq!(foreign.error().code(), "server_error");
    assert_eq!(foreign.exit_code(), 3);

    let domain = Failure::from(anyhow::Error::from(not_found!("gone")));
    assert_eq!(domain.exit_code(), 2);
}

#[test]
fn completions_bash_nonempty() {
    let response = completions::<App>(Shell::Bash, "app");

    assert_eq!(response.exit, 0);
    assert!(response.stderr.is_empty());
    let script = String::from_utf8(response.stdout).expect("completion script is utf-8");
    assert!(script.contains("_app"));
    assert!(script.contains("greet"));
}

#[test]
fn into_exit_passes_status() {
    assert_eq!(Response::success("").into_exit(), Ok(()));
    assert_eq!(Response::failure("", 2).into_exit(), Err(2));
    assert_eq!(Response::failure("", USAGE_EXIT).into_exit(), Err(64));
}

#[test]
fn from_env_mints_when_absent() {
    let metadata = Metadata::from_env("FROM_ENV_MINTS");

    let request_id = metadata.request_id.as_deref().expect("request id is minted");
    assert_eq!(request_id.len(), 32);
    assert!(request_id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(metadata.correlation_id, metadata.request_id);
    assert_eq!(metadata.causation_id, None);
    assert_ne!(Metadata::from_env("FROM_ENV_MINTS").request_id, metadata.request_id);
}

#[derive(Debug, Deserialize)]
struct Greet {
    name: String,
}

#[derive(Debug, Serialize)]
struct Greeting {
    text: String,
}

fn greet(input: Greet, _context: Context<()>) -> Ready<Result<Greeting, omnia_guest::Error>> {
    let Greet { name } = input;
    ready(Ok(Greeting {
        text: format!("hello, {name}"),
    }))
}

fn render_greeting(greeting: &Greeting, out: &mut dyn fmt::Write) -> fmt::Result {
    writeln!(out, "{}", greeting.text)
}

#[derive(Debug, Deserialize)]
struct Lookup {
    id: String,
}

fn lookup(input: Lookup, _context: Context<()>) -> Ready<Result<Greeting, omnia_guest::Error>> {
    let Lookup { id } = input;
    ready(Err(not_found!("no item {id}")))
}

fn render_never(_: &Greeting, _: &mut dyn fmt::Write) -> fmt::Result {
    panic!("a failure never renders the success body")
}

fn ada() -> Greet {
    Greet {
        name: "ada".to_string(),
    }
}

fn item(id: &str) -> Lookup {
    Lookup { id: id.to_string() }
}

fn stderr_json(response: &Response) -> serde_json::Value {
    serde_json::from_slice(&response.stderr).expect("stderr carries the JSON envelope")
}

#[tokio::test]
async fn call_success_text() {
    let client = Client::new("app", ());
    let metadata = Metadata::default();
    let response = Command::new(&client, &metadata, Format::Text)
        .call(greet, || Ok(ada()), render_greeting)
        .await;

    assert_eq!(response.exit, 0);
    assert!(response.stderr.is_empty());
    assert_eq!(response.stdout, b"hello, ada\n");
}

#[tokio::test]
async fn call_success_json_pretty_with_newline() {
    let client = Client::new("app", ());
    let metadata = Metadata::default();
    let response = Command::new(&client, &metadata, Format::Json)
        .call(greet, || Ok(ada()), render_greeting)
        .await;

    assert_eq!(response.exit, 0);
    let stdout = String::from_utf8(response.stdout).expect("json is utf-8");
    assert!(stdout.ends_with("}\n"));
    assert!(stdout.contains("\n  \"text\""));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout).expect("decode stdout"),
        serde_json::json!({ "text": "hello, ada" })
    );
}

#[tokio::test]
async fn call_handler_error_envelope_exit() {
    let client = Client::new("app", ());
    let metadata = Metadata::default();
    let command = Command::new(&client, &metadata, Format::Json);

    let response = command.call(lookup, || Ok(item("42")), render_never).await;

    assert_eq!(response.exit, 2);
    assert!(response.stdout.is_empty());
    assert_eq!(
        stderr_json(&response),
        serde_json::json!({ "error": "not_found", "message": "no item 42", "exit-code": 2 })
    );

    let text = Command::new(&client, &metadata, Format::Text)
        .call(lookup, || Ok(item("42")), render_never)
        .await;
    assert_eq!(text.exit, 2);
    assert_eq!(text.stderr, b"error[not_found]: no item 42\n");
}

#[tokio::test]
async fn call_decode_error_uses_error_exit() {
    let client = Client::new("app", ());
    let metadata = Metadata::default();
    let response = Command::new(&client, &metadata, Format::Json)
        .call(greet, || Err(bad_request!("name is required")), render_never)
        .await;

    assert_eq!(response.exit, 1);
    assert_eq!(stderr_json(&response)["error"], "bad_request");
    assert_eq!(stderr_json(&response)["exit-code"], 1);
}

#[tokio::test]
async fn hints_apply_by_error() {
    let client = Client::new("app", ());
    let metadata = Metadata::default();
    let command = Command::new(&client, &metadata, Format::Text).hints(|error| {
        (error.code() == "not_found").then_some(Cow::Borrowed("run `app list` to see the items"))
    });

    let hinted = command.call(lookup, || Ok(item("42")), render_never).await;
    assert_eq!(hinted.exit, 2);
    assert_eq!(
        hinted.stderr,
        b"error[not_found]: no item 42\nhint: run `app list` to see the items\n"
    );

    let unhinted =
        command.call(greet, || Err(bad_request!("name is required")), render_never).await;
    assert_eq!(unhinted.exit, 1);
    assert_eq!(unhinted.stderr, b"error[bad_request]: name is required\n");
}

#[cfg(feature = "http")]
mod http_parity {
    use axum::body::{Body, to_bytes};
    use http::{Request, StatusCode};
    use omnia_guest::api::ErrorBody;
    use omnia_guest::api::http::get;
    use tower::ServiceExt as _;

    use super::*;

    #[tokio::test]
    async fn handler_error_shares_http_discriminant() {
        let client = Client::new("app", ());
        let metadata = Metadata::default();
        let response = Command::new(&client, &metadata, Format::Json)
            .call(lookup, || Ok(item("42")), render_never)
            .await;
        let command_body: ErrorBody =
            serde_json::from_slice(&response.stderr).expect("stderr carries an error body");

        let router = axum::Router::new().route("/items", get(lookup)).with_state(client);
        let request =
            Request::builder().uri("/items?id=42").body(Body::empty()).expect("build request");
        let http = router.oneshot(request).await.expect("router serves request");
        assert_eq!(http.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(http.into_body(), usize::MAX).await.expect("collect body");
        let http_body: ErrorBody =
            serde_json::from_slice(&bytes).expect("http carries an error body");

        assert_eq!(command_body, http_body);
        assert_eq!(command_body.error, "not_found");
        assert_eq!(response.exit, 2);
    }
}
