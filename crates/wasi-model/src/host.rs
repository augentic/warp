//! # WASI Model Service
//!
//! Host side of the `omnia:model/completion` boundary. Follows the shared
//! host-crate shape (see `wasi-keyvalue`), adding a per-completion [`ToolHost`]
//! that the `create` binding assembles from the store's mounts and the
//! session channels it mints for the completion.

mod answer;
mod completion_impl;
mod default_impl;
mod request;
mod session;
mod tool_host;
mod workspace;

mod generated {
    #![allow(missing_docs)]

    pub use self::omnia::model::completion::Error;

    wasmtime::component::bindgen!({
        world: "model",
        path: "wit",
        imports: {
            default: store | tracing | trappable,
        },
        with: {
            "wasi:clocks": wasmtime_wasi::p3::bindings::clocks,
            "wasi:filesystem": wasmtime_wasi::p3::bindings::filesystem,
        },
        trappable_error_type: {
            "omnia:model/completion.error" => Error,
        },
    });
}

use std::fmt::Debug;
use std::sync::Arc;

pub use omnia_core::FutureResult;
use omnia_core::{HasMounts, Host, Server, StoreView};
use wasmtime::component::{HasData, Linker};

pub use self::answer::{Answer, ToolTurn, Transcript, Usage};
pub use self::default_impl::ModelDefault;
use self::generated::omnia::model::completion;
pub use self::generated::omnia::model::completion::{
    Effort, Error, Format, Function, Generation, Grants, Mcp, Message, Reply, Request, Role,
    Schema, Tool, WorkspaceGrant,
};
pub use self::session::Limits;
pub use self::tool_host::*;

/// Result type for model operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Host-side service for `wasi:model`.
#[derive(Debug)]
pub struct WasiModel;

impl HasData for WasiModel {
    type Data<'a> = WasiModelCtxView<'a>;
}

impl<T> Host<T> for WasiModel
where
    T: StoreView<Self> + HasMounts + 'static,
{
    fn add_to_linker(linker: &mut Linker<T>) -> anyhow::Result<()> {
        Ok(completion::add_to_linker::<_, Self>(linker, T::view)?)
    }
}

impl<B> Server<B> for WasiModel {}

/// A trait which provides internal WASI Model context.
///
/// This is implemented by the resource-specific provider of model
/// functionality. For example, the echo default, or a `genai`-backed
/// provider.
pub trait WasiModelCtx: Debug + Send + Sync + 'static {
    /// Call the model backend with a prompt to evaluate. [`ToolHost`]
    /// provides closure-like support for an in-process tool loop — the
    /// backend can use it to request more information from the host.
    fn complete(&self, request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer>;

    /// Session bounds the host enforces for this backend's completions.
    fn limits(&self) -> Limits {
        Limits::default()
    }
}

impl WasiModelCtx for Box<dyn WasiModelCtx> {
    fn complete(&self, request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer> {
        (**self).complete(request, tool_host)
    }

    fn limits(&self) -> Limits {
        (**self).limits()
    }
}

// An untyped host failure is a `backend` error at the boundary.
omnia_core::host_error!(Error, Backend);
omnia_core::wasi_view!(Model);
