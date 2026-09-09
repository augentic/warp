#![doc = include_str!("../README.md")]

pub mod api;
mod capabilities;
mod error;
#[cfg(feature = "http")]
pub mod mcp;
#[cfg(feature = "orm")]
pub mod orm;
mod provider;

/// Document store types and helpers (from `omnia-wasi-docstore`).
#[cfg(feature = "orm")]
pub mod document_store {
    pub use omnia_wasi_docstore::document_store::*;
}

#[cfg(feature = "http")]
#[doc(hidden)]
pub use axum;
/// The `schemars` a guest derives `JsonSchema` against, so the guest and
/// `model::Question<T>` share one version (derive with
/// `#[schemars(crate = "omnia_guest::schemars")]`).
#[cfg(feature = "schema")]
pub use schemars;
#[doc(hidden)]
pub use {anyhow, bytes, http, http_body, tracing};
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use {
    omnia_wasi_blobstore, omnia_wasi_http, omnia_wasi_identity, omnia_wasi_keyvalue,
    omnia_wasi_messaging, omnia_wasi_otel, wasip3, wit_bindgen,
};

#[cfg(feature = "http")]
pub use crate::api::http::{HttpError, HttpResult};
pub use crate::capabilities::*;
pub use crate::error::*;
