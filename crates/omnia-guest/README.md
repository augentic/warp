# Omnia Guest

Shared traits, error types, and abstractions for building WASI guest components. This crate provides the glue between your business logic and the Omnia runtime capabilities.

## Quick Start

A handler is an `async fn(I, Context<P>) -> Result<O, E>`. Write one, then register it with an explicit transport router.

```rust,ignore
use omnia_guest::api::http::post;
use omnia_guest::api::{Client, Context};
use omnia_guest::Error;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct CreateItem {
    name: String,
}

#[derive(Debug, Serialize)]
struct ItemResponse {
    id: String,
    name: String,
}

struct MyProvider;

async fn create_item(
    input: CreateItem, context: Context<MyProvider>,
) -> Result<ItemResponse, Error> {
    Ok(ItemResponse {
        id: format!("{}-123", context.owner()),
        name: input.name,
    })
}

fn router() -> axum::Router {
    axum::Router::new()
        .route("/items", post(create_item))
        .with_state(Client::new("my-org", MyProvider))
}
```

Every fn of that shape (and every `Clone` closure of it, so a closure can carry configuration) is a `Handler<P, I>` through a blanket impl; implementing the trait by hand on a local non-fn type is the escape hatch, not the norm. Name the fn for the operation (`create_item`) and keep the input DTO's noun (`CreateItem`).

`Client` owns the provider and builds the owned `Context` (`owner()`, `provider()`, public `metadata`) each call receives; `Context::new(owner, provider, metadata)` builds one directly for unit-testing a handler without a `Client`. The application owns its WASI export explicitly:

```rust,ignore
struct Http;
wasip3::http::service::export!(Http);

impl wasip3::exports::http::handler::Guest for Http {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        omnia_guest::api::http::serve(router(), request).await
    }
}
```

Omnia creates one WASI component instance per HTTP request. Construct the `axum::Router` with one provider-owning `Client` as its state inside each `handle` call; Axum's route-state clones share that client's `Arc<P>` only for that request. Durable application state belongs in host-side capabilities, not guest statics.

Messaging routes use the same handlers with exact topic registration:

```rust,ignore
use omnia_guest::api::messaging::{Router, consume};

let router = Router::new(Client::new("my-org", MyProvider))
    .route("items.created", consume(create_item));
```

`consume` decodes JSON and acknowledges successful output.

### Custom codecs

`get`/`post`/`put`/`patch`/`delete`/`consume` are JSON defaults: `get` and `delete` decode path and query parameters, while `post`/`put`/`patch` merge a JSON body with path parameters. When a route speaks another wire format (or needs other methods), supply the codec yourself: `handle_with(filter, handler, decode, encode)` pairs a `MethodFilter` (unions work, e.g. `MethodFilter::POST.or(MethodFilter::PUT)`) with a decoder `Fn(RawRequest<'_>) -> Result<I, DecodeError>` over the raw request (path parameters, query, headers, body) and an encoder `Fn(F::Output) -> Response` (reuse `axum::Json` for JSON output); `consume_with(handler, decode)` takes a decoder `Fn(&Delivery) -> Result<I, DecodeError>` over the whole delivery. Errors keep flowing through `Into<HttpError>`; `HttpError::with_body` carries a preformatted error body (e.g. an XML document) with its content type.

Command-mode guests parse argv with clap and call `Client::call(handler, input, &metadata)` on the same handlers. `omnia_guest::command!(entry)` wires an `async fn` returning `()` or `Result<(), u8>` as the `wasi:cli/run` export through `command::execute_wasi`, so guest telemetry is initialized and flushed. Omnia creates a fresh component instance for each command invocation.

## Capabilities

The guest crate exposes trait-based abstractions for host capabilities. When compiled to `wasm32`, these delegate to WASI host calls.

| Trait | Purpose |
| ----- | ------- |
| `Config` | Read configuration values from the host. |
| `HttpRequest` | Make outbound HTTP requests. |
| `Publish` | Publish messages to a topic. |
| `StateStore` | Get/set/delete key-value state with optional TTL, plus one-shot `cas` (conflict is a typed `CasError::Conflict`) and atomic `increment`. |
| `Identity` | Obtain access tokens from an identity provider. |
| `TableStore` | Execute SQL queries and statements via the ORM layer. |
| `Broadcast` | Send events over WebSocket channels. |
| `Plugins` | Request plugin loads through `omnia:plugins/loader`: name a package, a location, and an optional sha256 pin; receive a typed `Plugin` handle. The `plugins` module carries the shared `PluginRef`/`Digest` types, typed refusals convertible into `Error`, and `PluginCache` for ensure-once loads. |
| `BlobStore` / `BlobStoreExt` | Object storage. `BlobStore` is the ten primitives an implementor writes; `BlobStoreExt` (`has`, `delete_objects`, `clear`, `copy_object`, `move_object`) is derived for every `BlobStore` — one host call each on `wasm32`, composed from the primitives natively. |
| `DocumentStore` | Document CRUD and filtered queries. |
| `Model` | Prompt completions, with tool calls answered by a guest closure. |

Every trait is also implemented for `Arc<T>`, `&T`, and `Box<T>` where `T` implements it, so a capability may sit behind a shared handle and still satisfy a `P: Capability` bound.

### Example: Using Capabilities

```rust,ignore
use omnia_guest::{StateStore, Publish, Message};

async fn process(provider: &impl StateStore + Publish) -> anyhow::Result<()> {
    // Store some state
    provider.set("last_run", b"now", None).await?;

    // Publish a message
    let msg = Message::new(b"job_completed");
    provider.send("jobs.events", &msg).await?;

    Ok(())
}
```

## Error Handling

The crate provides an `Error` enum with HTTP-aware variants (`BadRequest`, `NotFound`, `ServerError`, `BadGateway`) and helper macros for ergonomic error creation.

```rust,ignore
use omnia_guest::{bad_request, server_error, not_found};

fn validate(name: &str) -> Result<(), omnia_guest::Error> {
    if name.is_empty() {
        return Err(bad_request!("name cannot be empty"));
    }
    Ok(())
}
```

## Architecture

See the [workspace documentation](https://github.com/augentic/omnia) for the full architecture guide.

## Cargo features

- `orm` *(default)*: the SQL ORM, table/document capabilities, and document-store re-exports.

Guests that do not use SQL or documents can disable defaults to shrink wasm build time and size.

## License

MIT OR Apache-2.0
