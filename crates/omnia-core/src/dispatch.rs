//! # Host-mediated dynamic linking
//!
//! A caller guest imports an interface (say `omnia:link/echo`) whose
//! implementation the host satisfies at runtime. The host polyfills that import
//! on the shared `Linker` so invoking it:
//!
//! 1. extracts a target identity from the call via a [`GuestSelector`],
//! 2. rejects any resource handle attempting to cross the seam,
//! 3. enforces a dispatch-depth bound,
//! 4. instantiates the target *fresh* on a new store and invokes the matching
//!    export over the bound wRPC transport, and
//! 5. returns the typed result, discarding the callee instance.
//!
//! Because step 4 is always a fresh instance, a dispatched call cannot
//! recursively re-enter its caller. The runtime core stays generic: it links whatever
//! interfaces the manifest names, by opaque string, and resolves opaque
//! [`GuestId`]s — it never parses a consumer scheme. The selector runs in the
//! polyfill *before* the call is encoded onto wRPC, so it sees typed
//! parameters. Sync-typed functions are registered with `func_new_async`;
//! async-typed (`async func`) ones with `func_new_concurrent`, whose
//! store-scoped access rules the decode path is built around. See
//! `docs/Architecture.md` (The Guest Registry) for the full design.

mod handle;
mod host;
mod link;
mod selector;
mod serve;
mod transport;
mod value;

pub use handle::DispatchHandle;
pub use host::Dispatcher;
// `polyfill_late`, `serve_guest`, and `Endpoint` are crate-internal: this
// module is private, and `lib.rs` does not re-export them.
pub use link::{WiredLinks, link, polyfill_late};
pub use selector::{FirstArgSelector, GuestSelector};
pub use serve::{serve_guest, serve_links};
pub use transport::{Endpoint, LinkClient, WrpcState};
