//! Handler invocation and HTTP routing contracts.

#![cfg(feature = "http")]

use std::future::{Ready, ready};
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::response::{IntoResponse, Response};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use omnia_guest::api::http::{
    HttpError, MethodFilter, RawRequest, delete, get, handle_with, patch, post, put,
};
use omnia_guest::api::{Client, Context, DecodeError, ErrorBody, Format, Metadata};
use omnia_guest::not_found;
use serde::{Deserialize, Serialize};
use tower::ServiceExt as _;

#[derive(Debug, Deserialize)]
struct EchoInput {
    name: String,
    count: Option<u32>,
}

#[derive(Debug, Serialize)]
struct EchoOutput {
    name: String,
    count: u32,
    owner: String,
    correlation_id: Option<String>,
}

fn echo<P: Send + Sync + 'static>(
    input: EchoInput, context: Context<P>,
) -> Ready<Result<EchoOutput, omnia_guest::Error>> {
    ready(Ok(EchoOutput {
        name: input.name,
        count: input.count.unwrap_or(1),
        owner: context.owner().to_owned(),
        correlation_id: context.metadata.correlation_id,
    }))
}

struct StatefulProvider {
    calls: AtomicUsize,
}

#[derive(Serialize)]
struct ProviderObservation {
    name: String,
    address: usize,
    call: usize,
}

#[derive(Debug, Deserialize)]
struct ObserveInput {
    name: String,
}

fn router() -> axum::Router {
    axum::Router::new()
        .route("/echo", get(echo))
        .route("/echo", post(echo))
        .route("/echo/{name}", get(echo))
        .route("/echo/{name}", post(echo))
        .route("/echo/{name}", put(echo))
        .route("/echo/{name}", patch(echo))
        .route("/echo/{name}", delete(echo))
        .with_state(Client::new("test", ()))
}

async fn send(request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router().oneshot(request).await.expect("router serves request");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("collect body");
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test]
async fn invoke() {
    let client = Client::new("tenant", ());
    let metadata = Metadata {
        correlation_id: Some("call-1".to_string()),
        ..Metadata::default()
    };

    let output = client
        .call(
            echo,
            EchoInput {
                name: "core".to_string(),
                count: None,
            },
            &metadata,
        )
        .await
        .expect("handler succeeds");

    assert_eq!(output.owner, "tenant");
    assert_eq!(output.correlation_id.as_deref(), Some("call-1"));
}

#[tokio::test]
async fn invoke_with_direct_context() {
    let context = Context::new("tenant", (), Metadata::default());

    let output = echo(
        EchoInput {
            name: "direct".to_string(),
            count: Some(2),
        },
        context,
    )
    .await
    .expect("handler succeeds");

    assert_eq!(output.owner, "tenant");
    assert_eq!(output.count, 2);
}

#[tokio::test]
async fn get_query() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/echo?name=plan&count=3")
        .header("x-request-id", "request-1")
        .body(Body::empty())
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value,
        serde_json::json!({
            "name": "plan",
            "count": 3,
            "owner": "test",
            "correlation_id": "request-1"
        })
    );
}

#[tokio::test]
async fn get_path_and_query() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/echo/slice?count=2")
        .body(Body::empty())
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["name"], "slice");
    assert_eq!(value["count"], 2);
}

#[tokio::test]
async fn get_missing_field() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/echo?count=3")
        .body(Body::empty())
        .expect("build request");
    let (status, _) = send(request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_body_and_path() {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/echo/slice")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"count":7}"#))
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["name"], "slice");
    assert_eq!(value["count"], 7);
}

#[tokio::test]
async fn post_empty_body() {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/echo/slice")
        .body(Body::empty())
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["count"], 1);
}

#[tokio::test]
async fn post_non_object_body() {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/echo/slice")
        .body(Body::from("[1,2]"))
        .expect("build request");
    let (status, _) = send(request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_body_and_path() {
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/echo/slice")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"count":4}"#))
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["name"], "slice");
    assert_eq!(value["count"], 4);
}

#[tokio::test]
async fn patch_body_and_path() {
    let request = Request::builder()
        .method(Method::PATCH)
        .uri("/echo/slice")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"count":5}"#))
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["name"], "slice");
    assert_eq!(value["count"], 5);
}

#[tokio::test]
async fn delete_path_and_query() {
    let request = Request::builder()
        .method(Method::DELETE)
        .uri("/echo/slice?count=6")
        .body(Body::empty())
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["name"], "slice");
    assert_eq!(value["count"], 6);
}

#[tokio::test]
async fn unregistered_method() {
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/echo")
        .body(Body::from(r#"{"name":"slice"}"#))
        .expect("build request");
    let (status, _) = send(request).await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn route_state_clones_share_provider() {
    let observe = |input: ObserveInput, context: Context<StatefulProvider>| {
        ready(Ok::<_, omnia_guest::Error>(ProviderObservation {
            name: input.name,
            address: std::ptr::from_ref(context.provider()).addr(),
            call: context.provider().calls.fetch_add(1, Ordering::SeqCst) + 1,
        }))
    };
    let router = axum::Router::new()
        .route("/first", get(observe))
        .route("/second", get(observe))
        .with_state(Client::new(
            "test",
            StatefulProvider {
                calls: AtomicUsize::new(0),
            },
        ));

    let first = router
        .clone()
        .oneshot(
            Request::builder().uri("/first?name=first").body(Body::empty()).expect("build request"),
        )
        .await
        .expect("first route serves request");
    let second = router
        .oneshot(
            Request::builder()
                .uri("/second?name=second")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("second route serves request");
    let first: serde_json::Value = serde_json::from_slice(
        &to_bytes(first.into_body(), usize::MAX).await.expect("collect first body"),
    )
    .expect("decode first body");
    let second: serde_json::Value = serde_json::from_slice(
        &to_bytes(second.into_body(), usize::MAX).await.expect("collect second body"),
    )
    .expect("decode second body");

    assert_eq!(first["name"], "first");
    assert_eq!(second["name"], "second");
    assert_eq!(first["address"], second["address"]);
    assert_eq!(first["call"], 1);
    assert_eq!(second["call"], 2);
}

#[derive(Debug, Deserialize)]
struct Welcome {
    name: String,
}

#[tokio::test]
async fn closure_handler_with_configuration() {
    let greeting = "welcome".to_string();
    let router = axum::Router::new()
        .route(
            "/welcome",
            get(move |input: Welcome, _context: Context<()>| async move {
                Ok::<_, omnia_guest::Error>(format!("{greeting}, {}", input.name))
            }),
        )
        .with_state(Client::new("test", ()));

    let request = Request::builder()
        .method(Method::GET)
        .uri("/welcome?name=ada")
        .body(Body::empty())
        .expect("build request");
    let response = router.oneshot(request).await.expect("router serves request");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("collect body");
    assert_eq!(serde_json::from_slice::<String>(&bytes).expect("decode body"), "welcome, ada");
}

#[derive(Debug)]
struct JoinNames {
    names: Vec<String>,
}

fn join_names<P: Send + Sync + 'static>(
    input: JoinNames, _context: Context<P>,
) -> Ready<Result<String, omnia_guest::Error>> {
    let JoinNames { names } = input;
    ready(Ok(names.join(" & ")))
}

#[derive(Debug)]
struct Greet {
    name: String,
    greeting: String,
}

fn greet<P: Send + Sync + 'static>(
    input: Greet, _context: Context<P>,
) -> Ready<Result<String, omnia_guest::Error>> {
    let Greet { name, greeting } = input;
    ready(Ok(format!("{greeting}, {name}")))
}

fn text_response(body: String) -> Response {
    (StatusCode::OK, [(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"))], body)
        .into_response()
}

fn text_router() -> axum::Router {
    axum::Router::new()
        .route(
            "/join",
            handle_with(
                MethodFilter::POST.or(MethodFilter::PUT),
                join_names,
                |raw: RawRequest<'_>| -> Result<JoinNames, DecodeError> {
                    let body = std::str::from_utf8(raw.body)
                        .map_err(|error| DecodeError::new(format!("body is not utf-8: {error}")))?;
                    Ok(JoinNames {
                        names: body.lines().map(str::to_owned).collect(),
                    })
                },
                text_response,
            ),
        )
        .route(
            "/greet/{name}",
            handle_with(
                MethodFilter::GET,
                greet,
                |raw: RawRequest<'_>| -> Result<Greet, DecodeError> {
                    let name = raw
                        .path_params
                        .iter()
                        .find(|(key, _)| key == "name")
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| DecodeError::new("missing name path parameter"))?;
                    let greeting = raw
                        .headers
                        .get("x-greeting")
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| DecodeError::new("missing x-greeting header"))?;
                    Ok(Greet {
                        name,
                        greeting: greeting.to_owned(),
                    })
                },
                text_response,
            ),
        )
        .with_state(Client::new("test", ()))
}

async fn send_raw(
    router: axum::Router, request: Request<Body>,
) -> (StatusCode, Option<String>, String) {
    let response = router.oneshot(request).await.expect("router serves request");
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("collect body");
    (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn text_codec_post() {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/join")
        .body(Body::from("ada\ngrace"))
        .expect("build request");
    let (status, content_type, body) = send_raw(text_router(), request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(body, "ada & grace");
}

#[tokio::test]
async fn method_filter_union() {
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/join")
        .body(Body::from("ada\ngrace"))
        .expect("build request");
    let (status, _, body) = send_raw(text_router(), request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ada & grace");
}

#[tokio::test]
async fn text_codec_path_and_header() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/greet/ada")
        .header("x-greeting", "hello")
        .body(Body::empty())
        .expect("build request");
    let (status, content_type, body) = send_raw(text_router(), request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(body, "hello, ada");
}

#[tokio::test]
async fn text_codec_missing_header() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/greet/ada")
        .body(Body::empty())
        .expect("build request");
    let (status, _, _) = send_raw(text_router(), request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[derive(Debug)]
struct XmlGreet {
    name: String,
}

#[derive(Debug)]
struct EmptyName;

impl std::fmt::Display for EmptyName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("name cannot be empty")
    }
}

impl std::error::Error for EmptyName {}

impl From<EmptyName> for HttpError {
    fn from(error: EmptyName) -> Self {
        let body = format!("<error><code>empty_name</code><message>{error}</message></error>");
        Self::with_body(
            StatusCode::UNPROCESSABLE_ENTITY,
            HeaderValue::from_static("text/xml; charset=utf-8"),
            body.into_bytes(),
        )
    }
}

fn xml_greet<P: Send + Sync + 'static>(
    input: XmlGreet, _context: Context<P>,
) -> Ready<Result<String, EmptyName>> {
    let XmlGreet { name } = input;
    ready(if name.is_empty() { Err(EmptyName) } else { Ok(format!("hello, {name}")) })
}

fn parse_greet(headers: &HeaderMap, body: &[u8]) -> Result<XmlGreet, DecodeError> {
    let content_type = headers.get(CONTENT_TYPE).and_then(|value| value.to_str().ok());
    if content_type != Some("text/xml") {
        return Err(DecodeError::new("expected a text/xml request"));
    }
    let body = std::str::from_utf8(body)
        .map_err(|error| DecodeError::new(format!("body is not utf-8: {error}")))?;
    let start = body.find("<name>").map(|index| index + "<name>".len());
    let end = body.find("</name>");
    match (start, end) {
        (Some(start), Some(end)) if start <= end => Ok(XmlGreet {
            name: body[start..end].to_owned(),
        }),
        _ => Err(DecodeError::new("malformed greet document")),
    }
}

fn xml_router() -> axum::Router {
    axum::Router::new()
        .route(
            "/greet",
            handle_with(
                MethodFilter::POST,
                xml_greet,
                |raw: RawRequest<'_>| parse_greet(raw.headers, raw.body),
                |greeting: String| {
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, HeaderValue::from_static("text/xml; charset=utf-8"))],
                        format!("<greeting>{greeting}</greeting>"),
                    )
                        .into_response()
                },
            ),
        )
        .with_state(Client::new("xml", ()))
}

fn xml_request(body: &'static str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/greet")
        .header(CONTENT_TYPE, "text/xml")
        .body(Body::from(body))
        .expect("build request")
}

#[tokio::test]
async fn xml_success() {
    let (status, content_type, body) =
        send_raw(xml_router(), xml_request("<greet><name>ada</name></greet>")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/xml; charset=utf-8"));
    assert_eq!(body, "<greeting>hello, ada</greeting>");
}

#[tokio::test]
async fn xml_handler_error() {
    let (status, content_type, body) =
        send_raw(xml_router(), xml_request("<greet><name></name></greet>")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(content_type.as_deref(), Some("text/xml; charset=utf-8"));
    assert_eq!(
        body,
        "<error><code>empty_name</code><message>name cannot be empty</message></error>"
    );
}

#[tokio::test]
async fn xml_malformed_body() {
    let (status, content_type, body) = send_raw(xml_router(), xml_request("<greet>")).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    let body: ErrorBody = serde_json::from_str(&body).expect("decode error body");
    assert_eq!(body.error, "invalid_request");
    assert_eq!(body.message, "malformed greet document");
}

#[derive(Debug, Deserialize)]
struct Lookup {
    id: String,
}

fn lookup<P: Send + Sync + 'static>(
    input: Lookup, _context: Context<P>,
) -> Ready<Result<String, omnia_guest::Error>> {
    let Lookup { id } = input;
    ready(Err(not_found!("no item {id}")))
}

#[tokio::test]
async fn error_body_json() {
    let router =
        axum::Router::new().route("/items", get(lookup)).with_state(Client::new("test", ()));
    let request = Request::builder()
        .method(Method::GET)
        .uri("/items?id=42")
        .body(Body::empty())
        .expect("build request");
    let (status, content_type, body) = send_raw(router, request).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).expect("decode error body"),
        serde_json::json!({ "error": "not_found", "message": "no item 42" })
    );
}

#[derive(Debug, Serialize)]
struct Summary {
    name: String,
    count: u32,
}

fn render_summary(summary: &Summary, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
    write!(out, "{} x{}", summary.name, summary.count)
}

#[test]
fn format_encode_text_and_json() {
    let summary = Summary {
        name: "widget".to_string(),
        count: 3,
    };

    let text = Format::Text.encode(&summary, render_summary);
    assert_eq!(text.media_type, "text/plain; charset=utf-8");
    assert_eq!(text.bytes, b"widget x3");

    let json = Format::Json.encode(&summary, render_summary);
    assert_eq!(json.media_type, "application/json");
    let json = String::from_utf8(json.bytes).expect("json is utf-8");
    assert!(json.ends_with('\n'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).expect("decode json"),
        serde_json::json!({ "name": "widget", "count": 3 })
    );
}

#[derive(Debug, Deserialize)]
struct Summarize {
    name: String,
    count: u32,
}

fn summarize<P: Send + Sync + 'static>(
    input: Summarize, _context: Context<P>,
) -> Ready<Result<Summary, omnia_guest::Error>> {
    ready(Ok(Summary {
        name: input.name,
        count: input.count,
    }))
}

#[tokio::test]
async fn encoded_into_response() {
    let router = axum::Router::new()
        .route(
            "/summary",
            handle_with(
                MethodFilter::GET,
                summarize,
                |raw: RawRequest<'_>| {
                    serde_urlencoded::from_str::<Summarize>(raw.query.unwrap_or_default())
                        .map_err(|error| DecodeError::new(error.to_string()))
                },
                |summary: Summary| Format::Text.encode(&summary, render_summary).into_response(),
            ),
        )
        .with_state(Client::new("test", ()));
    let request = Request::builder()
        .method(Method::GET)
        .uri("/summary?name=widget&count=3")
        .body(Body::empty())
        .expect("build request");
    let (status, content_type, body) = send_raw(router, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(body, "widget x3");
}

fn is_minted_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[tokio::test]
async fn missing_request_id_is_minted() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/echo?name=plan")
        .body(Body::empty())
        .expect("build request");
    let (status, value) = send(request).await;

    assert_eq!(status, StatusCode::OK);
    let correlation_id = value["correlation_id"].as_str().expect("correlation id is minted");
    assert!(is_minted_id(correlation_id), "{correlation_id:?} is not 32 lowercase hex chars");
}

#[test]
fn from_lookup_mints_distinct_request_ids() {
    let first = Metadata::from_lookup(|_| None);
    let second = Metadata::from_lookup(|_| None);

    let request_id = first.request_id.as_deref().expect("request id is minted");
    assert!(is_minted_id(request_id));
    assert_eq!(first.correlation_id, first.request_id);
    assert_eq!(first.causation_id, None);
    assert_ne!(first.request_id, second.request_id);
    assert_eq!(Metadata::default().request_id, None);
}

#[test]
fn from_lookup_reads_every_id() {
    let metadata = Metadata::from_lookup(|name| match name {
        "request-id" => Some("req-1".to_owned()),
        "correlation-id" => Some("corr-1".to_owned()),
        "causation-id" => Some("cause-1".to_owned()),
        _ => None,
    });

    assert_eq!(metadata.request_id.as_deref(), Some("req-1"));
    assert_eq!(metadata.correlation_id.as_deref(), Some("corr-1"));
    assert_eq!(metadata.causation_id.as_deref(), Some("cause-1"));
    assert_eq!(metadata.deadline, None);
}

#[test]
fn from_lookup_correlation_falls_back_to_request_id() {
    let metadata = Metadata::from_lookup(|name| (name == "request-id").then(|| "req-2".to_owned()));

    assert_eq!(metadata.request_id.as_deref(), Some("req-2"));
    assert_eq!(metadata.correlation_id.as_deref(), Some("req-2"));
    assert_eq!(metadata.causation_id, None);
}

#[derive(Debug)]
struct Fetch {
    id: String,
}

fn fetch<P: Send + Sync + 'static>(
    input: Fetch, _context: Context<P>,
) -> Ready<Result<String, omnia_guest::Error>> {
    let Fetch { id } = input;
    ready(Ok(format!("item {id}")))
}

#[tokio::test]
async fn decoder_classifies_not_found() {
    let router = axum::Router::new()
        .route(
            "/items/{id}",
            handle_with(
                MethodFilter::GET,
                fetch,
                |raw: RawRequest<'_>| -> Result<Fetch, omnia_guest::Error> {
                    let id = raw
                        .path_params
                        .iter()
                        .find(|(key, value)| key == "id" && value.parse::<u32>().is_ok())
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| not_found!("no such item"))?;
                    Ok(Fetch { id })
                },
                text_response,
            ),
        )
        .with_state(Client::new("test", ()));

    let (status, _, body) = send_raw(
        router.clone(),
        Request::builder().uri("/items/7").body(Body::empty()).expect("build request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "item 7");

    let (status, content_type, body) = send_raw(
        router,
        Request::builder().uri("/items/seven").body(Body::empty()).expect("build request"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    let body: ErrorBody = serde_json::from_str(&body).expect("decode error body");
    assert_eq!(body.error, "not_found");
    assert_eq!(body.message, "no such item");
}
