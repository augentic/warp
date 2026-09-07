//! Typed messaging routing over application handlers.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use serde::de::DeserializeOwned;

use crate::api::{Client, DecodeError, Handler, Metadata};

/// An owned inbound delivery independent of a messaging binding.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Delivery {
    /// The exact topic supplied by the host.
    pub topic: Option<String>,
    /// The opaque message payload.
    pub payload: Vec<u8>,
    /// The optional payload media type.
    pub content_type: Option<String>,
    /// Transport metadata in delivery order.
    pub metadata: Vec<(String, String)>,
}

/// A delivery failure projected onto the current WIT error result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryError {
    /// The delivery had no topic.
    MissingTopic,
    /// No route was registered for the exact topic.
    UnhandledTopic(String),
    /// Decoding or the handler rejected the delivery.
    Rejected(String),
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTopic => f.write_str("message is missing topic"),
            Self::UnhandledTopic(topic) => write!(f, "unhandled topic: {topic}"),
            Self::Rejected(error) => f.write_str(error),
        }
    }
}

impl Error for DeliveryError {}

/// A typed messaging route awaiting a topic.
pub struct Consume<F, I, D> {
    handler: F,
    decode: D,
    marker: PhantomData<fn() -> I>,
}

/// Create a messaging route with a custom delivery decoder.
#[must_use]
pub fn consume_with<F, I, D>(handler: F, decode: D) -> Consume<F, I, D>
where
    D: Fn(&Delivery) -> Result<I, DecodeError> + Send + Sync + 'static,
{
    Consume {
        handler,
        decode,
        marker: PhantomData,
    }
}

/// Create a JSON-decoded messaging route.
#[must_use]
pub fn consume<F, I: DeserializeOwned>(
    handler: F,
) -> Consume<F, I, impl Fn(&Delivery) -> Result<I, DecodeError> + Send + Sync + 'static> {
    consume_with(handler, |delivery: &Delivery| {
        serde_json::from_slice(&delivery.payload)
            .map_err(|error| DecodeError::new(format!("malformed JSON payload: {error}")))
    })
}

type DispatchFuture<'a> = Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send + 'a>>;

trait ErasedRoute<P: Send + Sync + 'static>: Send + Sync {
    fn dispatch<'a>(&'a self, delivery: &'a Delivery, client: &'a Client<P>) -> DispatchFuture<'a>;
}

struct Route<P, F, I, D> {
    handler: F,
    decode: D,
    marker: PhantomData<fn(P) -> I>,
}

impl<P, F, I, D> ErasedRoute<P> for Route<P, F, I, D>
where
    P: Send + Sync + 'static,
    F: Handler<P, I>,
    I: Send + 'static,
    D: Fn(&Delivery) -> Result<I, DecodeError> + Send + Sync + 'static,
{
    fn dispatch<'a>(&'a self, delivery: &'a Delivery, client: &'a Client<P>) -> DispatchFuture<'a> {
        Box::pin(async move {
            let input = (self.decode)(delivery)
                .map_err(|error| DeliveryError::Rejected(error.to_string()))?;
            let metadata = Metadata::from_lookup(|name| {
                delivery
                    .metadata
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.clone())
            });
            client
                .call(self.handler.clone(), input, &metadata)
                .await
                .map(|_| ())
                .map_err(|error| DeliveryError::Rejected(error.to_string()))
        })
    }
}

/// An exact-topic messaging router.
pub struct Router<P: Send + Sync + 'static> {
    client: Client<P>,
    routes: BTreeMap<String, Arc<dyn ErasedRoute<P>>>,
}

impl<P: Send + Sync + 'static> Router<P> {
    /// Create an empty router backed by a client.
    #[must_use]
    pub fn new(client: Client<P>) -> Self {
        Self {
            client,
            routes: BTreeMap::new(),
        }
    }

    /// Register one handler for one exact topic.
    ///
    /// # Panics
    ///
    /// Panics when the topic is empty or already registered.
    #[must_use]
    pub fn route<F, I, D>(mut self, topic: impl Into<String>, consume: Consume<F, I, D>) -> Self
    where
        F: Handler<P, I>,
        I: Send + 'static,
        D: Fn(&Delivery) -> Result<I, DecodeError> + Send + Sync + 'static,
    {
        let topic = topic.into();
        assert!(!topic.is_empty(), "messaging topic cannot be empty");
        assert!(!self.routes.contains_key(&topic), "duplicate messaging topic `{topic}`");
        self.routes.insert(
            topic,
            Arc::new(Route::<P, F, I, D> {
                handler: consume.handler,
                decode: consume.decode,
                marker: PhantomData,
            }),
        );
        self
    }

    /// Dispatch one delivery by exact topic.
    ///
    /// # Errors
    ///
    /// Returns missing-topic, unhandled-topic, decoding, or handler failures.
    pub async fn handle(&self, delivery: Delivery) -> Result<(), DeliveryError> {
        let topic = delivery.topic.as_deref().ok_or(DeliveryError::MissingTopic)?;
        let route = self
            .routes
            .get(topic)
            .ok_or_else(|| DeliveryError::UnhandledTopic(topic.to_owned()))?;
        route.dispatch(&delivery, &self.client).await
    }
}

/// Adapt a WIT message to an owned delivery and dispatch it.
///
/// The current WIT contract carries only `result<_, error>`: success
/// acknowledges the delivery, while every router failure is returned as
/// `error.other` for the host to interpret.
///
/// # Errors
///
/// Returns the WIT delivery failure.
#[cfg(target_arch = "wasm32")]
pub async fn handle<P: Send + Sync + 'static>(
    router: &Router<P>, message: omnia_wasi_messaging::types::Message,
) -> Result<(), omnia_wasi_messaging::types::Error> {
    let delivery = Delivery {
        topic: message.topic(),
        payload: message.data(),
        content_type: message.content_type(),
        metadata: message.metadata().unwrap_or_default(),
    };
    router
        .handle(delivery)
        .await
        .map_err(|error| omnia_wasi_messaging::types::Error::Other(error.to_string()))
}
