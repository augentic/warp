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
- `omnia-guest` — guest SDK: the handler contract, HTTP/messaging routers and the command façade, error types, ORM helpers, MCP support
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

Typed routes default to JSON: `http::get` and `http::delete` decode path and query parameters, while `http::post`, `http::put`, and `http::patch` merge a JSON body with path parameters. Routes that speak another wire format (or need other methods) use the general constructors instead: `http::handle_with(filter, handler, decode, encode)` pairs a `MethodFilter` (unions work, e.g. `MethodFilter::POST.or(MethodFilter::PUT)`) with a decoder `Fn(RawRequest<'_>) -> Result<I, E>` (any `E: Into<HttpError>`, so a decoder can classify its refusal — a `not_found!` path parameter answers 404) over a raw-request view (path parameters, query, headers, body) plus an encoder `Fn(F::Output) -> Response`, and `messaging::consume_with(handler, decode)` takes a decoder `Fn(&Delivery) -> Result<I, DecodeError>` over the whole `Delivery`. Errors keep flowing through `Into<HttpError>`: an `omnia_guest::Error` becomes the JSON `api::ErrorBody` (`{"error","message"}`) at the variant's status, and `HttpError::with_body` carries a preformatted error body (e.g. an XML document) with its content type.

## Command-mode guests

For run-once workloads (jobs, CLIs, agent tasks), `omnia_guest::api::command` is the command-line mirror of the HTTP router over the same [handler contract](#the-handler-contract): argv is decoded into handler input, the handler runs through `Client::call`, and the output is encoded onto the process channels. The clap-backed parts (`parse`, `completions`, the `clap::ValueEnum` derive on `Format`) sit behind the `command` cargo feature (`omnia-guest = { version = "...", features = ["command"] }`); the projector itself needs no feature.

```rust,noplayground
use std::fmt;

use clap::{Args, Parser, Subcommand};
use omnia_guest::Error;
use omnia_guest::api::command::{Command, Parsed, Response, parse};
use omnia_guest::api::{Client, Context, Format, Metadata};
use serde::Serialize;

#[derive(Parser)]
struct App {
    /// Output format for every verb
    #[arg(long, global = true, default_value = "text")]
    format: Format,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    Sync(SyncInput),
}

#[derive(Args)]
struct SyncInput {
    source: String,
}

#[derive(Serialize)]
struct Synced {
    count: usize,
}

async fn sync(input: SyncInput, context: Context<MyProvider>) -> Result<Synced, Error> {
    // ...
}

fn render_synced(synced: &Synced, out: &mut dyn fmt::Write) -> fmt::Result {
    writeln!(out, "synced {} items", synced.count)
}

omnia_guest::command!(main);

async fn main() -> Response {
    let app = match parse::<App>(wasip3::cli::environment::get_arguments()) {
        Parsed::App(app) => app,
        Parsed::Display(text) => return Response::success(text),
        Parsed::Usage(error) => return Response::usage(&error),
    };
    let client = Client::new("acme", MyProvider);
    let metadata = Metadata::from_env("APP");
    let command = Command::new(&client, &metadata, app.format)
        .hints(|error| (error.code() == "not_found").then(|| "run `app sync --help`".into()));
    match app.verb {
        Verb::Sync(input) => command.call(sync, || Ok(input), render_synced).await,
    }
}
```

The pieces, in the order argv flows through them:

- **`parse::<App>(argv) -> Parsed`** classifies clap's outcomes: the grammar (`Parsed::App`), help or version text for stdout at exit 0 (`Parsed::Display`), or a usage error (`Parsed::Usage`), which `Response::usage(&error)` renders on stderr at `USAGE_EXIT` (64, `EX_USAGE`). `completions::<App>(shell, name)` produces a shell-completion script as a `Response`; `Shell` is re-exported from `clap_complete`.
- **`Command::new(&client, &metadata, format)`** is the projector, and `command.call(handler, decode, render)` runs one verb. `decode: FnOnce() -> Result<I, Error>` builds the handler input from the parsed grammar (its `bad_request!` exits 1 like any other refusal); the handler runs through `Client::call`; a success body is encoded in the selected `Format` onto stdout at exit 0 — `render` is its text form (`Fn(&T, &mut dyn fmt::Write) -> fmt::Result`), and `Format::Json` is the output pretty-printed with a trailing newline. `.hints(|error| ..)` attaches a remedy hint to every failure that carries none.
- **`Response { stdout, stderr, exit }`** buffers both channels. `command!(main)` binds an `async fn main() -> Response` as the `wasi:cli/run` export and writes the channels at that boundary through `IntoExit`: a `BrokenPipe` on either channel (a reader that went away) keeps the response's own exit, any other refused channel exits 3. Because the channels are whole buffers, a command that must stream writes to the process channels itself and returns `Result<(), u8>` or `()`, which `command!` also accepts; a guest that needs its own export writes the `export!` and calls `command::execute_wasi` itself.
- **`Metadata::from_env(prefix)`** reads `<PREFIX>_REQUEST_ID`, `<PREFIX>_CORRELATION_ID`, and `<PREFIX>_CAUSATION_ID` from the environment. A missing request id is minted from `wasi:random`, as on every transport, so a command invocation is as observable as an HTTP request: `Client::call` runs the handler in a `handler` span carrying the request and correlation ids.

### Failures and the exit map

A decode or handler error becomes a `Failure` envelope on stderr, in the same `Format` as a success body, and the process exits with `Error::exit_code()`:

| Error class | `error` (from the macros) | Exit | HTTP status |
| ----------- | ------------------------- | ---- | ----------- |
| `BadRequest` | `bad_request` | 1 | 400 |
| `NotFound` | `not_found` | 2 | 404 |
| `ServerError` | `server_error` | 3 | 500 |
| `BadGateway` | `bad_gateway` | 4 | 502 |
| clap usage error | — | 64 | — |

As text the envelope is `error[<code>]: <message>` followed by `hint: <hint>` when a hint is attached; as JSON it is flat: `{"error","message","exit-code","hint"?}` (the hint key is omitted when absent). `error` and `message` are the transport-neutral `api::ErrorBody` — `HttpError::from(Error)` emits the same two fields as an `application/json` body at the variant's status — so a client reads one discriminant whether it reached the handler over HTTP or a shell. Usage errors exit 64 rather than clap's default 2, so exit 2 always means a `NotFound` envelope.

- Arguments after `--` on the host command line arrive as the guest's argv (`args[0]` is the program name, supplied by the runtime).
- The host runtime must be built with `mode: command` — see [Composing a Runtime](composing-a-runtime.md).
- The telemetry lifecycle imports `omnia:otel`, so the deployment links `WasiOtel` (the no-op `OtelDefault` suffices).

`examples/cli` is the runnable pair: `greet`, `add`, `env`, and a `fail [CLASS]` verb that returns each error class so the exit map can be observed.

## Tracing

Annotate functions with `#[omnia_wasi_otel::instrument]` to wrap them in an OpenTelemetry span. `tracing::debug!` and friends work inside guests; spans flow to whatever OTel backend the host configures:

```rust
#[omnia_wasi_otel::instrument(name = "http_guest_handle", level = Level::INFO)]
async fn handle(request: Request) -> Result<Response, ErrorCode> { /* ... */ }
```

## Serving MCP tools

A guest can act as an [MCP](https://modelcontextprotocol.io) (Model Context Protocol) server — exposing tools and resources to AI agents over HTTP. Implement `omnia_guest::mcp::McpServer` and serve `mcp::router` from your HTTP handler; see [Model Completions and MCP](model-completions.md#serving-mcp-tools-from-a-guest).
