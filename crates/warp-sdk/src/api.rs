//! # API
//!
//! The api module provides the entry point to the public API. Requests are routed
//! to the appropriate handler for processing, returning a response that can
//! be serialized to a JSON object or directly to HTTP.
//!
//! ## Example Usage
//!
//! ```rust,ignore
//! use warp_sdk::{Body, Client, Headers};
//!
//! // Create a client (typestate builder)
//! let client = Client::new("alice").provider(provider);
//!
//! // Simple request without headers
//! let response = client.request(my_request).await?;
//!
//! // Request with headers
//! let response = client.request(my_request).headers(my_headers).await?;
//! ```

mod into_http;
mod reply;
mod request;

use std::fmt::Debug;
use std::sync::Arc;

pub use self::into_http::*;
pub use self::reply::*;
pub use self::request::*;

pub trait Provider: Send + Sync {}
impl<T> Provider for T where T: Send + Sync {}

/// Typestate marker indicating a [`Client`] has not yet been configured with a provider.
///
/// Calling `.provider(...)` transitions `Client<NoProvider>` into `Client<Arc<P>>`, and
/// request methods are only available on the configured state.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProvider;

/// Build an API client to execute the request.
///
/// The client is the main entry point for making API requests. It holds
/// the provider configuration and provides methods to create the request
/// router.
#[derive(Clone, Debug)]
pub struct Client<P = NoProvider> {
    /// The owning tenant/namespace.
    owner: Arc<str>,

    /// The provider to use while handling of the request.
    provider: Arc<P>,
}

impl Client<NoProvider> {
    /// Start building a new `Client` by setting the owner.
    #[must_use]
    pub fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: Arc::<str>::from(owner.into()),
            provider: Arc::new(NoProvider),
        }
    }

    /// Finish building the client by providing the provider implementation.
    #[must_use]
    pub fn provider<P: Provider>(self, provider: P) -> Client<P> {
        Client {
            owner: self.owner,
            provider: Arc::new(provider),
        }
    }
}

impl<P: Provider> Client<P> {
    /// Create a new [`RequestHandler`] with no headers.
    pub fn request<R: Handler<P>>(&self, request: R) -> RequestHandler<R, P> {
        RequestHandler::from_client(self, request)
    }
}

/// The `Body` trait is used to restrict the types able to implement
/// request body. It is implemented by all `xxxRequest` types.
pub trait Body: Debug + Send + Sync {}
impl<T> Body for T where T: Debug + Send + Sync {}
