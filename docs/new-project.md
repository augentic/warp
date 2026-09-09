# Starting a New Project

[Getting Started](getting-started.md) runs the examples inside this repository. This page sets up a **standalone project**: your own workspace with a guest crate and a host crate, depending on the published `omnia-*` crates.

## Prerequisites

- **Rust 1.97 or later** with the `wasm32-wasip2` target. Pin both in a `rust-toolchain.toml` at your workspace root so every checkout gets them automatically:

```toml
[toolchain]
channel = "stable"
targets = ["wasm32-wasip2"]
```

## Workspace layout

Two crates — the guest (compiles to `.wasm`) and the host (a native binary):

```text
my-app/
├── Cargo.toml          # workspace
├── rust-toolchain.toml
├── guest/
│   ├── Cargo.toml
│   └── src/lib.rs
└── host/
    ├── Cargo.toml
    └── src/main.rs
```

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "3"
members = ["guest", "host"]
```

## The guest crate

A guest is a `cdylib` library targeting `wasm32-wasip2`:

```toml
# guest/Cargo.toml
[package]
name = "my-guest"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
anyhow = "1"
axum = { version = "0.8", default-features = false, features = ["json"] }
omnia-guest = "0.35"
omnia-wasi-http = "0.35"
serde_json = "1"
wasip3 = { version = "0.7", features = ["http-compat"] }
wit-bindgen = { version = "0.60", features = ["async-spawn"] }
```

```rust
// guest/src/lib.rs
#![cfg(target_arch = "wasm32")]

use axum::routing::get;
use axum::{Json, Router};
use omnia_guest::HttpResult;
use serde_json::{Value, json};
use wasip3::exports::http::handler::Guest;
use wasip3::http::types::{ErrorCode, Request, Response};

struct HttpGuest;
wasip3::http::service::export!(HttpGuest);

impl Guest for HttpGuest {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let router = Router::new().route("/", get(hello).post(echo));
        omnia_wasi_http::serve(router, request).await
    }
}

async fn hello() -> HttpResult<Json<Value>> {
    Ok(Json(json!({ "message": "hello from the guest" })))
}

async fn echo(Json(body): Json<Value>) -> HttpResult<Json<Value>> {
    Ok(Json(json!({ "echo": body })))
}
```

Keep `wasip3` and `wit-bindgen` on the same versions the omnia release pins — a mismatch causes handler deadlocks (see [Troubleshooting](troubleshooting.md#outbound-http-or-spawned-work-inside-a-handler-deadlocks)).

## The host crate

The host is a normal binary; the `runtime!` macro generates its `main`:

```toml
# host/Cargo.toml
[package]
name = "my-host"
version = "0.1.0"
edition = "2024"

[dependencies]
omnia = "0.35"
omnia-wasi-http = "0.35"
omnia-wasi-otel = "0.35"
```

```rust
// host/src/main.rs
use omnia_wasi_http::{HttpDefault, WasiHttp};
use omnia_wasi_otel::{OtelDefault, WasiOtel};

omnia::runtime!({
    hosts: {
        WasiHttp: HttpDefault,
        WasiOtel: OtelDefault,
    }
});
```

Note the same `omnia-wasi-http` crate appears in both manifests: it compiles to guest bindings on `wasm32` and to the host implementation on native targets.

That dependency list is complete. `omnia` is the facade over the runtime — the crates behind it (`omnia-core`, `omnia-link`, `omnia-plugin`) are never named in a host's `Cargo.toml` or source, even when the deployment declares a `link:` or `plugin:` block or a hand-written `Backend`; everything the macro emits resolves through `omnia::…`, and it re-exports `anyhow` (`omnia::anyhow::Result`) and `futures` (`omnia::futures::future::BoxFuture`) for the `Backend`/`Wiring` and plugin-store signatures that speak them.

## Build and run

Two steps, always in this order — guest first, then host:

```bash
cargo build -p my-guest --target wasm32-wasip2
RUST_LOG=info cargo run -p my-host -- run ./target/wasm32-wasip2/debug/my_guest.wasm
```

(The artifact name uses underscores: `my_guest.wasm`.) When `omnia ready` appears, the server is on `localhost:8080`:

```bash
curl http://localhost:8080
curl -X POST http://localhost:8080 -H "Content-Type: application/json" -d '{"hello":"world"}'
```

## Matching versions

Keep every `omnia*` dependency on the same release line (all `0.35`, for example) — the WIT contracts must agree between guest bindings and host implementations. If the line you need is not yet on crates.io, point the dependencies at the git repository instead:

```toml
[patch.crates-io]
omnia = { git = "https://github.com/augentic/omnia.git" }
omnia-guest = { git = "https://github.com/augentic/omnia.git" }
omnia-wasi-http = { git = "https://github.com/augentic/omnia.git" }
omnia-wasi-otel = { git = "https://github.com/augentic/omnia.git" }
```

Production backends come from the separate [`omnia-backends`](https://github.com/augentic/omnia-backends) repository; its release lines pair with specific omnia lines (see [Production Backends](guides/production-backends.md)).

## Where to go next

- **[Writing Guests](guides/writing-guests.md)** — every guest-side pattern: capabilities, messaging, command mode, tracing.
- **[Composing a Runtime](guides/composing-a-runtime.md)** — modes, backends, and compiled-in deployments for the host.
- **[omnia-exemplar](https://github.com/augentic/omnia-exemplar)** — a complete application-scale reference service built this way.
