//! Typed HTTP routing over application handlers.
//!
//! Routes are plain [`axum::routing::MethodRouter`]s over a [`Client`] state:
//! register them on an [`axum::Router`], then supply exactly one
//! provider-owning [`Client`] via `with_state`. [`handle_with`] is the general
//! constructor, pairing a [`MethodFilter`] with a decode/encode codec; the
//! per-verb constructors ([`get`], [`post`], [`put`], [`patch`], [`delete`])
//! are JSON conveniences over it. Omnia creates one component instance per
//! HTTP request, so construct the router and client inside the WASI HTTP
//! export's `handle` method; axum route-state clones share that client's
//! provider allocation, and durable state belongs in host-side capabilities.

use std::convert::Infallible;
use std::future::{Future, ready};

use axum::body::Bytes;
use axum::extract::{FromRequestParts, RawPathParams, RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
pub use axum::routing::MethodFilter;
use axum::routing::{self, MethodRouter};
use http::header::CONTENT_TYPE;
use http::request::Parts;
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use crate::api::DecodeError;
use crate::api::{Client, Handler, Metadata};

/// Result type for HTTP handlers.
pub type HttpResult<T, E = HttpError> = Result<T, E>;

impl From<DecodeError> for HttpError {
    fn from(error: DecodeError) -> Self {
        crate::Error::BadRequest {
            code: "invalid_request".to_string(),
            description: error.description().to_owned(),
        }
        .into()
    }
}

/// An HTTP error response.
#[derive(Debug)]
pub struct HttpError {
    status: StatusCode,
    error: String,
    body: Option<(HeaderValue, Vec<u8>)>,
}

impl HttpError {
    /// Create a plain-text HTTP error.
    #[must_use]
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            error: message.into(),
            body: None,
        }
    }

    /// Create an HTTP error carrying a preformatted wire body.
    #[must_use]
    pub const fn with_body(status: StatusCode, content_type: HeaderValue, body: Vec<u8>) -> Self {
        Self {
            status,
            error: String::new(),
            body: Some((content_type, body)),
        }
    }
}

impl From<crate::Error> for HttpError {
    fn from(error: crate::Error) -> Self {
        Self::new(error.status(), error.to_string())
    }
}

impl From<anyhow::Error> for HttpError {
    fn from(error: anyhow::Error) -> Self {
        if error.downcast_ref::<crate::Error>().is_some() {
            let error: crate::Error = error.into();
            return Self::from(error);
        }

        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{error}, caused by: {}", error.root_cause()),
        )
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        match self.body {
            Some((content_type, body)) => {
                (self.status, [(CONTENT_TYPE, content_type)], body).into_response()
            }
            None => (self.status, self.error).into_response(),
        }
    }
}

/// Serve one WASI HTTP request through a finished [`axum::Router`].
#[cfg(target_arch = "wasm32")]
pub use omnia_wasi_http::serve;

/// A borrowed raw-request view handed to route decoders.
#[derive(Debug)]
pub struct RawRequest<'a> {
    /// Path parameters in path order.
    pub path_params: &'a [(String, String)],

    /// The raw query string, when present.
    pub query: Option<&'a str>,

    /// The request headers.
    pub headers: &'a HeaderMap,

    /// The raw request body (empty for typical GETs).
    pub body: &'a [u8],
}

impl<S: Send + Sync> FromRequestParts<S> for Metadata {
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts, _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> {
        let metadata = Self::from_lookup(|name| {
            parts
                .headers
                .get(format!("x-{name}"))
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        });
        ready(Ok(metadata))
    }
}

/// Create a route for the filtered methods with a custom decoder and encoder.
///
/// ```rust,ignore
/// use axum::response::IntoResponse;
/// use omnia_guest::api::{Client, Context};
/// use omnia_guest::api::http::{DecodeError, MethodFilter, RawRequest, handle_with};
///
/// async fn import_text(input: ImportText, context: Context<Provider>) -> Result<String, Error> {
///     // ...
/// }
///
/// let router = axum::Router::new()
///     .route(
///         "/import",
///         handle_with(
///             MethodFilter::POST.or(MethodFilter::PUT),
///             import_text,
///             |raw: RawRequest<'_>| {
///                 let text = std::str::from_utf8(raw.body)
///                     .map_err(|error| DecodeError::new(format!("body is not utf-8: {error}")))?;
///                 Ok(ImportText { text: text.to_owned() })
///             },
///             |output: String| output.into_response(),
///         ),
///     )
///     .with_state(Client::new("acme", Provider));
/// ```
pub fn handle_with<F, P, I, D, E>(
    filter: MethodFilter, handler: F, decode: D, encode: E,
) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Error: Into<HttpError>,
    I: Send + 'static,
    D: Fn(RawRequest<'_>) -> Result<I, DecodeError> + Clone + Send + Sync + 'static,
    E: Fn(F::Output) -> Response + Clone + Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    routing::on(
        filter,
        move |state: State<Client<P>>,
              params: RawPathParams,
              query: RawQuery,
              headers: HeaderMap,
              metadata: Metadata,
              body: Bytes| {
            dispatch(
                (handler.clone(), decode.clone(), encode.clone()),
                state,
                params,
                query,
                headers,
                metadata,
                body,
            )
        },
    )
}

/// Create a GET route decoding path and query parameters as JSON input.
pub fn get<F, P, I>(handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    json_query(MethodFilter::GET, handler)
}

/// Create a DELETE route decoding path and query parameters as JSON input.
pub fn delete<F, P, I>(handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    json_query(MethodFilter::DELETE, handler)
}

/// Create a POST route decoding a JSON body merged with path parameters.
pub fn post<F, P, I>(handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    json_body(MethodFilter::POST, handler)
}

/// Create a PUT route decoding a JSON body merged with path parameters.
pub fn put<F, P, I>(handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    json_body(MethodFilter::PUT, handler)
}

/// Create a PATCH route decoding a JSON body merged with path parameters.
pub fn patch<F, P, I>(handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    json_body(MethodFilter::PATCH, handler)
}

fn json_query<F, P, I>(filter: MethodFilter, handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    handle_with(
        filter,
        handler,
        |raw: RawRequest<'_>| query_input::<I>(raw.path_params, raw.query),
        json_response,
    )
}

fn json_body<F, P, I>(filter: MethodFilter, handler: F) -> MethodRouter<Client<P>>
where
    F: Handler<P, I>,
    F::Output: Serialize,
    F::Error: Into<HttpError>,
    I: DeserializeOwned + Send + 'static,
    P: Send + Sync + 'static,
{
    handle_with(
        filter,
        handler,
        |raw: RawRequest<'_>| body_input::<I>(raw.path_params, raw.body),
        json_response,
    )
}

fn json_response<T: Serialize>(output: T) -> Response {
    Json(output).into_response()
}

async fn dispatch<F, P, I, D, E>(
    (handler, decode, encode): (F, D, E), State(client): State<Client<P>>, params: RawPathParams,
    RawQuery(query): RawQuery, headers: HeaderMap, metadata: Metadata, body: Bytes,
) -> Response
where
    F: Handler<P, I>,
    F::Error: Into<HttpError>,
    D: Fn(RawRequest<'_>) -> Result<I, DecodeError>,
    E: Fn(F::Output) -> Response,
    P: Send + Sync + 'static,
{
    let path_params: Vec<(String, String)> =
        params.iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect();
    let raw = RawRequest {
        path_params: &path_params,
        query: query.as_deref(),
        headers: &headers,
        body: &body,
    };
    let input = match decode(raw) {
        Ok(input) => input,
        Err(error) => return HttpError::from(error).into_response(),
    };
    match client.call(handler, input, &metadata).await {
        Ok(output) => encode(output),
        Err(error) => Into::<HttpError>::into(error).into_response(),
    }
}

fn invalid(description: String) -> DecodeError {
    DecodeError::new(description)
}

fn query_input<T: DeserializeOwned>(
    params: &[(String, String)], query: Option<&str>,
) -> Result<T, DecodeError> {
    let mut pairs: Vec<(String, String)> = params.to_vec();
    if let Some(query) = query {
        let parsed: Vec<(String, String)> = serde_urlencoded::from_str(query)
            .map_err(|error| invalid(format!("malformed query string: {error}")))?;
        pairs.extend(parsed);
    }
    let encoded = serde_urlencoded::to_string(&pairs)
        .map_err(|error| invalid(format!("cannot encode request parameters: {error}")))?;
    serde_urlencoded::from_str(&encoded)
        .map_err(|error| invalid(format!("invalid request parameters: {error}")))
}

fn body_input<T: DeserializeOwned>(
    params: &[(String, String)], body: &[u8],
) -> Result<T, DecodeError> {
    let mut value = if body.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(body)
            .map_err(|error| invalid(format!("malformed JSON body: {error}")))?
    };
    let serde_json::Value::Object(object) = &mut value else {
        return Err(invalid("the request body must be a JSON object".to_string()));
    };
    for (key, param) in params {
        object.insert(key.clone(), serde_json::Value::String(param.clone()));
    }
    serde_json::from_value(value).map_err(|error| invalid(format!("invalid request body: {error}")))
}
