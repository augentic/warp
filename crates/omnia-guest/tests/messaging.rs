//! Exact-topic messaging routing contracts.

use std::future::{Ready, ready};

use omnia_guest::api::messaging::{
    Delivery, DeliveryError, Router as MessagingRouter, consume, consume_with,
};
use omnia_guest::api::{Client, Context, DecodeError};
use serde::{Deserialize, Serialize};

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

fn raw_text<P: Send + Sync + 'static>(
    input: RawText, _context: Context<P>,
) -> Ready<Result<String, omnia_guest::Error>> {
    ready(Ok(input.text))
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
