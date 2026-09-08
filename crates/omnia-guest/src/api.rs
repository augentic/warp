//! Transport-neutral handler invocation and transport adapters.
//!
//! Application logic is a [`Handler`]: any `async fn(I, Context<P>)`
//! returning a `Result`. [`Client`] owns the provider and builds the
//! [`Context`] each call receives.
//!
//! ```rust,ignore
//! use std::convert::Infallible;
//!
//! use omnia_guest::api::{Client, Context, Metadata};
//!
//! struct Provider;
//!
//! struct Greet {
//!     name: String,
//! }
//!
//! async fn greet(input: Greet, context: Context<Provider>) -> Result<String, Infallible> {
//!     Ok(format!("hello, {} from {}", input.name, context.owner()))
//! }
//!
//! async fn run() -> String {
//!     let client = Client::new("acme", Provider);
//!     client.call(greet, Greet { name: "omnia".into() }, &Metadata::default()).await.unwrap()
//! }
//! ```

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tracing::Instrument as _;

pub mod command;
pub mod http;
/// Typed exact-topic messaging routing.
pub mod messaging;

/// An input that could not be decoded into handler input.
#[derive(Debug)]
pub struct DecodeError {
    description: String,
}

impl DecodeError {
    /// Create a decode error from its description.
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }

    /// Describe why the input could not be decoded.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.description)
    }
}

impl Error for DecodeError {}

/// The transport-neutral wire body of a failed invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The error code discriminant.
    pub error: String,

    /// The human-readable error description.
    pub message: String,
}

impl From<&crate::Error> for ErrorBody {
    fn from(error: &crate::Error) -> Self {
        Self {
            error: error.code(),
            message: error.description(),
        }
    }
}

/// The wire format a handler output is encoded into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "command", derive(clap::ValueEnum))]
pub enum Format {
    /// Human-readable text produced by a render fn.
    Text,

    /// Pretty-printed JSON with a trailing newline.
    Json,
}

/// An encoded body with its media type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Encoded {
    /// The encoded body bytes.
    pub bytes: Vec<u8>,

    /// The media type describing `bytes`.
    pub media_type: &'static str,
}

impl Format {
    /// Encode a body, rendering it through `render` for [`Format::Text`].
    ///
    /// # Panics
    ///
    /// Panics if `render` reports an error (a `String` sink never does) or
    /// `body` fails to serialize; both are invariant violations for a
    /// derived DTO encoded into memory, not runtime conditions.
    pub fn encode<T: Serialize>(
        self, body: &T, render: impl Fn(&T, &mut dyn fmt::Write) -> fmt::Result,
    ) -> Encoded {
        match self {
            Self::Text => {
                let mut out = String::new();
                render(body, &mut out).expect("a String sink never fails");
                Encoded {
                    bytes: out.into_bytes(),
                    media_type: "text/plain; charset=utf-8",
                }
            }
            Self::Json => {
                let mut bytes = serde_json::to_vec_pretty(body).expect("a derived DTO serializes");
                bytes.push(b'\n');
                Encoded {
                    bytes,
                    media_type: "application/json",
                }
            }
        }
    }
}

/// Transport-neutral invocation metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    /// Identifies this invocation at its transport boundary.
    pub request_id: Option<String>,

    /// Correlates work across transport and capability boundaries.
    pub correlation_id: Option<String>,

    /// Identifies the invocation that directly caused this work.
    pub causation_id: Option<String>,

    /// The latest instant at which the caller considers the work useful.
    pub deadline: Option<SystemTime>,
}

impl Metadata {
    /// Build metadata from a transport's named-value lookup.
    ///
    /// Names are the transport-neutral `request-id` / `correlation-id` /
    /// `causation-id`. A missing request id is minted so every invocation
    /// is observable; the correlation id falls back to the request id.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let request_id = lookup("request-id").unwrap_or_else(mint_request_id);
        Self {
            correlation_id: Some(lookup("correlation-id").unwrap_or_else(|| request_id.clone())),
            request_id: Some(request_id),
            causation_id: lookup("causation-id"),
            deadline: None,
        }
    }

    /// Mint metadata for a transport-initiated invocation.
    ///
    /// The freshly minted request id doubles as the correlation id.
    #[must_use]
    pub fn minted(request_id: String) -> Self {
        Self {
            correlation_id: Some(request_id.clone()),
            request_id: Some(request_id),
            causation_id: None,
            deadline: None,
        }
    }
}

// A fresh request id: 32 lowercase hex chars from 128 random bits.
fn mint_request_id() -> String {
    let (high, low) = random_u64_pair();
    format!("{high:016x}{low:016x}")
}

#[cfg(target_arch = "wasm32")]
fn random_u64_pair() -> (u64, u64) {
    use wasip3::random::random::get_random_u64;

    (get_random_u64(), get_random_u64())
}

// The native path exists so the transports are testable off-target; it
// is unique per call but not a CSPRNG, and no native transport mints ids.
#[cfg(not(target_arch = "wasm32"))]
fn random_u64_pair() -> (u64, u64) {
    use std::hash::{BuildHasher, RandomState};

    (RandomState::new().hash_one(0_u8), RandomState::new().hash_one(1_u8))
}

/// Context owned by one handler call.
#[derive(Clone, Debug)]
pub struct Context<P> {
    owner: Arc<str>,
    provider: Arc<P>,

    /// Transport-neutral invocation metadata.
    pub metadata: Metadata,
}

impl<P> Context<P> {
    /// Create a context outside a [`Client`], typically to unit-test a handler.
    pub fn new(owner: impl Into<String>, provider: P, metadata: Metadata) -> Self {
        Self {
            owner: Arc::from(owner.into()),
            provider: Arc::new(provider),
            metadata,
        }
    }

    /// Return the owning tenant or namespace.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Return the provider used to fulfil the call.
    #[must_use]
    pub fn provider(&self) -> &P {
        &self.provider
    }
}

/// An application handler: any `async fn(I, Context<P>) -> Result<O, E>`.
///
/// Every such fn (and every `Clone` closure of that shape) is a `Handler`
/// through the blanket impl; implement the trait by hand only for a
/// non-fn type.
pub trait Handler<P, I>: Clone + Send + Sync + 'static {
    /// The typed handler output.
    type Output: Send;

    /// The handler failure.
    type Error: Error + Send + Sync + 'static;

    /// Execute the handler.
    ///
    /// # Errors
    ///
    /// Returns the handler's error.
    fn call(
        self, input: I, context: Context<P>,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

// `I` is a trait parameter rather than an associated type: an associated
// `Input` would be unconstrained in this blanket impl (E0207).
impl<F, Fut, P, I, O, E> Handler<P, I> for F
where
    F: FnOnce(I, Context<P>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<O, E>> + Send,
    O: Send,
    E: Error + Send + Sync + 'static,
{
    type Error = E;
    type Output = O;

    fn call(
        self, input: I, context: Context<P>,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send {
        self(input, context)
    }
}

/// Provider-owning handler client.
///
/// Clones share one provider allocation. Transports define its lifetime; HTTP
/// constructs one client per WASI request and keeps durable state host-side.
pub struct Client<P> {
    owner: Arc<str>,
    provider: Arc<P>,
}

impl<P: Send + Sync + 'static> Client<P> {
    /// Create a client with one clone-shared provider allocation.
    pub fn new(owner: impl Into<String>, provider: P) -> Self {
        Self {
            owner: Arc::from(owner.into()),
            provider: Arc::new(provider),
        }
    }

    /// Return the owning tenant or namespace.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Return the shared provider.
    #[must_use]
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Invoke a handler with the given input and metadata inside a `handler`
    /// tracing span carrying the request and correlation ids.
    ///
    /// # Errors
    ///
    /// Returns the handler's error.
    pub async fn call<F, I>(
        &self, handler: F, input: I, metadata: &Metadata,
    ) -> Result<F::Output, F::Error>
    where
        F: Handler<P, I>,
    {
        let span = tracing::info_span!(
            "handler",
            request_id = metadata.request_id.as_deref(),
            correlation_id = metadata.correlation_id.as_deref(),
        );
        let context = Context {
            owner: Arc::clone(&self.owner),
            provider: Arc::clone(&self.provider),
            metadata: metadata.clone(),
        };
        handler.call(input, context).instrument(span).await
    }
}

impl<P: Send + Sync + 'static> Clone for Client<P> {
    fn clone(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            provider: Arc::clone(&self.provider),
        }
    }
}

impl<P: Send + Sync + 'static> fmt::Debug for Client<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("owner", &self.owner).finish_non_exhaustive()
    }
}
