//! Handler invocation and HTTP routing contracts.

// The handler contract is async; these test bodies just have nothing to await.
#![allow(clippy::unused_async)]

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::response::{IntoResponse, Response};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use omnia_guest::api::http::{
    HttpError, MethodFilter, RawRequest, delete, get, handle_with, patch, post, put,
};
use omnia_guest::api::messaging::{
    Delivery, DeliveryError, Router as MessagingRouter, consume, consume_with,
};
use omnia_guest::api::{Client, Context, DecodeError, Metadata};
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

async fn echo<P: Send + Sync + 'static>(
    input: EchoInput, context: Context<P>,
) -> Result<EchoOutput, omnia_guest::Error> {
    Ok(EchoOutput {
        name: input.name,
        count: input.count.unwrap_or(1),
        owner: context.owner().to_owned(),
        correlation_id: context.metadata.correlation_id.clone(),
    })
}

struct StatefulProvider {
    calls: AtomicUsize,
}

#[derive(Serialize)]
struct ProviderObservation {
    address: usize,
    call: usize,
}

#[derive(Debug, Deserialize)]
struct ObserveInput {
    name: String,
}

async fn observe(
    input: ObserveInput, context: Context<StatefulProvider>,
) -> Result<ProviderObservation, omnia_guest::Error> {
    let _ = input.name;
    Ok(ProviderObservation {
        address: std::ptr::from_ref(context.provider()).addr(),
        call: context.provider().calls.fetch_add(1, Ordering::SeqCst) + 1,
    })
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

fn delivery(topic: Option<&str>, payload: &[u8]) -> Delivery {
    Delivery {
        topic: topic.map(str::to_owned),
        payload: payload.to_vec(),
        content_type: Some("application/json".to_string()),
        metadata: vec![("correlation-id".to_string(), "delivery-1".to_string())],
    }
}

#[tokio::test]
async fn messaging_exact_topic() {
    let router =
        MessagingRouter::new(Client::new("messages", ())).route("events.created", consume(echo));

    router
        .handle(delivery(Some("events.created"), br#"{"name":"message","count":2}"#))
        .await
        .expect("exact route handles delivery");
    assert_eq!(
        router.handle(delivery(Some("events.*"), br#"{"name":"message"}"#)).await,
        Err(DeliveryError::UnhandledTopic("events.*".to_string()))
    );
}

#[tokio::test]
async fn messaging_failures() {
    let router = MessagingRouter::new(Client::new("messages", ())).route("events", consume(echo));

    assert_eq!(
        router.handle(delivery(None, br#"{"name":"message"}"#)).await,
        Err(DeliveryError::MissingTopic)
    );
    assert!(matches!(
        router.handle(delivery(Some("events"), b"not-json")).await,
        Err(DeliveryError::Rejected(_))
    ));
}

#[test]
#[should_panic(expected = "duplicate messaging topic")]
fn messaging_duplicate_topic() {
    let _router = MessagingRouter::new(Client::new("messages", ()))
        .route("events", consume(echo))
        .route("events", consume(echo));
}

#[derive(Debug)]
struct RawText {
    text: String,
}

async fn raw_text<P: Send + Sync + 'static>(
    input: RawText, _context: Context<P>,
) -> Result<(), omnia_guest::Error> {
    let _ = input.text;
    Ok(())
}

#[tokio::test]
async fn consume_with_raw_payload() {
    let router = MessagingRouter::new(Client::new("messages", ())).route(
        "events.raw",
        consume_with(raw_text, |delivery: &Delivery| {
            std::str::from_utf8(&delivery.payload)
                .map(|text| RawText {
                    text: text.to_owned(),
                })
                .map_err(|error| DecodeError::new(format!("payload is not utf-8: {error}")))
        }),
    );

    router
        .handle(delivery(Some("events.raw"), b"not-json"))
        .await
        .expect("raw decoder handles a payload JSON consume rejects");
}

#[derive(Debug)]
struct JoinNames {
    names: Vec<String>,
}

async fn join_names<P: Send + Sync + 'static>(
    input: JoinNames, _context: Context<P>,
) -> Result<String, omnia_guest::Error> {
    Ok(input.names.join(" & "))
}

#[derive(Debug)]
struct Greet {
    name: String,
    greeting: String,
}

async fn greet<P: Send + Sync + 'static>(
    input: Greet, _context: Context<P>,
) -> Result<String, omnia_guest::Error> {
    Ok(format!("{}, {}", input.greeting, input.name))
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
                |raw: RawRequest<'_>| {
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
                |raw: RawRequest<'_>| {
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

async fn xml_greet<P: Send + Sync + 'static>(
    input: XmlGreet, _context: Context<P>,
) -> Result<String, EmptyName> {
    if input.name.is_empty() { Err(EmptyName) } else { Ok(format!("hello, {}", input.name)) }
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
    let (status, content_type, _) = send_raw(xml_router(), xml_request("<greet>")).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(content_type.as_deref(), Some("text/plain; charset=utf-8"));
}
