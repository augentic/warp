# Writing Guests

A guest is your application logic compiled to a WebAssembly component. This guide is an **overview**: it shows each guest-side pattern once and links to the deep dive. Choose your path:

- [Project setup](#project-setup) — the crate shape every guest shares
- [HTTP handlers](#http-handlers) — serve requests with Axum
- [Using WASI capabilities](#using-wasi-capabilities) — storage, messaging, SQL, models, and the rest
- [Handling incoming messages](#handling-incoming-messages) — the messaging trigger
- [The handler contract](#the-handler-contract) — Handler + Client + Context behind typed HTTP/messaging routing and CLI dispatch
- [Command-mode guests](#command-mode-guests) — run-once jobs and CLIs
- [Tracing](#tracing) — spans and logs from inside the sandbox
- [Serving MCP tools](#serving-mcp-tools) — expose tools to AI agents

Every pattern here is drawn from a runnable pair in [`examples/`](../../examples/).

## Project setup

A guest is a `cdylib` crate targeting `wasm32-wasip2`. Guest code is guarded with `#[cfg(target_arch = "wasm32")]` so the same workspace also compiles for the host triple:

```rust
#![cfg(target_arch = "wasm32")]
```

Typical guest dependencies:

- `wasip3` — WASI Preview 3 bindings (exports, HTTP types, CLI, filesystem preopens)
- `omnia-guest` — guest SDK: `HttpResult`, error types, ORM helpers, MCP support
- `omnia-wasi-*` — the guest side of each capability you use (`omnia-wasi-keyvalue`, `omnia-wasi-messaging`, ...). These crates compile to guest bindings on `wasm32` and to the host implementation on native, so hosts and guests share one dependency name.

A minimal HTTP guest crate looks like this (align `wasip3`/`wit-bindgen` with the versions the omnia workspace pins — a mismatch causes executor deadlocks, see [Troubleshooting](../troubleshooting.md#outbound-http-or-spawned-work-inside-a-handler-deadlocks)):

```toml
[package]
name = "my-guest"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
anyhow = "1"
axum = { version = "0.8", default-features = false, features = ["json"] }
omnia-guest = "0.35"
omnia-wasi-http = "0.35"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
wasip3 = { version = "0.7", features = ["http-compat"] }
wit-bindgen = { version = "0.60", features = ["async-spawn"] }
```

Build with:

```bash
cargo build --example <name>-wasm --target wasm32-wasip2
# output: target/wasm32-wasip2/debug/examples/<name>_wasm.wasm  (underscores)
```

## HTTP handlers

Export the WASI HTTP handler and hand routing to [Axum](https://github.com/tokio-rs/axum) via `omnia_wasi_http::serve`:

```rust,noplayground
struct HttpGuest;
wasip3::http::service::export!(HttpGuest);

impl Guest for HttpGuest {
    #[omnia_wasi_otel::instrument(name = "http_guest_handle", level = Level::DEBUG)]
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let router = Router::new().route("/", get(echo_get)).route("/", post(echo_post));
        omnia_wasi_http::serve(router, request).await
    }
}
```

Handlers are ordinary Axum handlers. Return `omnia_guest::HttpResult<T>` to map errors to HTTP responses; `anyhow::Context` works as usual.

For **outbound** HTTP requests, use `omnia_wasi_http::handle` with a standard `http::Request` (see `examples/http-proxy` and the messaging example's upstream call).

## Using WASI capabilities

Each capability is a module in its `omnia-wasi-*` crate. The guest never names an implementation — the host decides what backs each interface.

Key-value (`wasi:keyvalue`):

```rust,noplayground
let bucket = store::open("omnia_bucket".to_string()).await.context("opening bucket")?;

bucket.set("my_key".to_string(), body.to_vec()).await.context("storing data")?;

let res = bucket.get("my_key".to_string()).await.context("reading data")?;
```

Publishing a message (`wasi:messaging`):

```rust
let client = Client::connect("default".to_string()).await?;
let message = Message::new(&payload);
producer::send(&client, "my-topic".to_string(), message).await?;
```

The other capabilities follow the same shape; each has a full example:

| Capability | Guest module | Example | Deep dive |
| ---------- | ------------ | ------- | --------- |
| Key-value | `omnia_wasi_keyvalue::store` | `examples/keyvalue` | — |
| Messaging | `omnia_wasi_messaging::{producer, request_reply}` | `examples/messaging` | [Messaging](messaging.md) |
| SQL + ORM | `omnia_wasi_sql` (with `entity!`) | `examples/sql` | [SQL and the ORM](sql-and-orm.md) |
| Document store | `omnia_wasi_docstore` | `examples/docstore` | [Document Store](document-store.md) |
| Blob store | `omnia_wasi_blobstore` | `examples/blobstore` | — |
| Secrets | `omnia_wasi_vault` | `examples/vault` | — |
| Config | `omnia_wasi_config` | `examples/config` | — |
| Identity/OAuth | `omnia_wasi_identity` | `examples/identity` | — |
| Model completions | `omnia_wasi_model::completion` | `examples/model` | [Model Completions](model-completions.md) |
| WebSockets | `omnia_wasi_websocket` | `examples/websocket` | [Messaging § WebSockets](messaging.md#websockets) |

## Handling incoming messages

A guest can export a messaging handler alongside (or instead of) an HTTP handler. The host's messaging trigger delivers each subscribed message to it:

```rust,noplayground
pub struct Messaging;
omnia_wasi_messaging::export!(Messaging with_types_in omnia_wasi_messaging);

impl omnia_wasi_messaging::incoming_handler::Guest for Messaging {
    async fn handle(message: Message) -> anyhow::Result<(), Error> {
        omnia_guest::api::messaging::handle(&router(), message).await
    }
}
```

`examples/messaging` demonstrates pub-sub, request-reply, and fan-out with the in-memory default backend; the same guest works against Kafka or NATS.

## The handler contract

`omnia-guest` keeps application logic independent of how it is invoked. Three pieces, defined once and reused by every transport:

- A **handler** is one unit of application work: an `async fn(I, Context<P>) -> Result<O, E>` named for the operation (e.g. `create_item`), taking the input DTO by value (e.g. `CreateItem`).
- A **provider** is the struct your handlers run against — it carries their capabilities (implement `DocumentStore`, `Config`, etc. on it).
- A **client** (`Client::new(owner, provider)`) binds the provider to an owner id and calls handlers.

Write a handler as a plain `async fn`. The first parameter is the owned input; the second is an owned `Context<P>` exposing `owner()`, `provider()`, and the public transport-neutral `metadata`:

```rust,noplayground
async fn create_item<P>(input: CreateItem, context: Context<P>) -> Result<ItemReply, Error>
where
    P: Config + StateStore,
{
    // context.provider() carries the capabilities in the fn's bounds
}
```

Every fn of that shape is a `Handler<P, I>` through a blanket impl, as is any `Clone` closure of it — so a closure can carry configuration into a route. Implementing `Handler<P, I>` by hand on a local non-fn type is the escape hatch for the rare case a fn cannot express. A mis-shaped fn is reported by rustc's own diagnostics at the route or `Client::call` site. To unit-test a handler without a `Client`, build its context with `Context::new(owner, provider, metadata)`.

For a guest that runs on the WASI-backed capability defaults, declare the provider with `omnia_guest::provider!` instead of writing one empty impl per capability (the expansion compiles on `wasm32` only; native tests supply mock providers):

```rust,noplayground
omnia_guest::provider! {
    /// Bare provider backed by the default WASI capability implementations.
    pub struct Provider: Config + HttpRequest + Identity + Publish + StateStore;
}
```

Routers then map transport events onto handlers: HTTP routes are plain `axum::routing::MethodRouter`s registered on an `axum::Router` whose state is one provider-owning `Client`, and a messaging router maps exact topics. Your WASI export stays visible application code — it just hands the event to the router:

```rust
fn router() -> axum::Router {
    axum::Router::new()
        .route("/api/items", post(create_item))
        .with_state(Client::new("acme-corp", MyProvider))
}

impl Guest for Http {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        omnia_guest::api::http::serve(router(), request).await
    }
}
```

Messaging uses `api::messaging::Router` and `consume(create_item)`; topic matching is exact. The export remains visible application code and calls `api::messaging::handle`. Because the same handler fns register in any router, one guest can expose the same logic over HTTP, messaging, and a CLI without duplicating it.

Typed routes default to JSON: `http::get` and `http::delete` decode path and query parameters, while `http::post`, `http::put`, and `http::patch` merge a JSON body with path parameters. Routes that speak another wire format (or need other methods) use the general constructors instead: `http::handle_with(filter, handler, decode, encode)` pairs a `MethodFilter` (unions work, e.g. `MethodFilter::POST.or(MethodFilter::PUT)`) with a decoder `Fn(RawRequest<'_>) -> Result<I, DecodeError>` over a raw-request view (path parameters, query, headers, body) plus an encoder `Fn(F::Output) -> Response`, and `messaging::consume_with(handler, decode)` takes a decoder `Fn(&Delivery) -> Result<I, DecodeError>` over the whole `Delivery`. Errors keep flowing through `Into<HttpError>`, and `HttpError::with_body` carries a preformatted error body (e.g. an XML document) with its content type.

## Command-mode guests

For run-once workloads (jobs, CLIs, agent tasks), parse argv with clap and call `Client::call(handler, input, &metadata)` on the same [handler contract](#the-handler-contract). `omnia_guest::command!` wires an `async fn` returning `()` or `Result<(), u8>` as the `wasi:cli/run` export, run through `command::execute_wasi` so guest telemetry is initialized and flushed; a guest that needs its own export writes the `export!` and calls `execute_wasi` itself:

```rust,noplayground
use clap::Parser;
use omnia_guest::api::{Client, Context, Metadata};

#[derive(Parser)]
enum App {
    Sync(SyncInput),
}

async fn sync(input: SyncInput, context: Context<MyProvider>) -> Result<String, Error> {
    // ...
}

omnia_guest::command!(dispatch);

async fn dispatch() -> Result<(), u8> {
    let app = App::try_parse_from(wasip3::cli::environment::get_arguments()).map_err(|error| {
        let _ = error.print();
        2
    })?;
    let client = Client::new("acme", MyProvider);
    let output = match app {
        App::Sync(input) => client.call(sync, input, &Metadata::default()).await.map_err(|_| 1)?,
    };
    print!("{output}");
    Ok(())
}
```

- Arguments after `--` on the host command line arrive as the guest's argv (`args[0]` is the program name, supplied by the runtime).
- clap parses argv into handler input; the guest prints output and maps handler failures to an exit code.
- clap supplies nested help, version, and usage handling.
- The host runtime must be built with `mode: command` — see [Composing a Runtime](composing-a-runtime.md).
- The telemetry lifecycle imports `omnia:otel`, so the deployment links `WasiOtel` (the no-op `OtelDefault` suffices).

## Tracing

Annotate functions with `#[omnia_wasi_otel::instrument]` to wrap them in an OpenTelemetry span. `tracing::debug!` and friends work inside guests; spans flow to whatever OTel backend the host configures:

```rust
#[omnia_wasi_otel::instrument(name = "http_guest_handle", level = Level::INFO)]
async fn handle(request: Request) -> Result<Response, ErrorCode> { /* ... */ }
```

## Serving MCP tools

A guest can act as an [MCP](https://modelcontextprotocol.io) (Model Context Protocol) server — exposing tools and resources to AI agents over HTTP. Implement `omnia_guest::mcp::McpServer` and serve `mcp::router` from your HTTP handler; see [Model Completions and MCP](model-completions.md#serving-mcp-tools-from-a-guest).
