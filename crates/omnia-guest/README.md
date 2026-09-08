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

`get`/`post`/`put`/`patch`/`delete`/`consume` are JSON defaults: `get` and `delete` decode path and query parameters, while `post`/`put`/`patch` merge a JSON body with path parameters. When a route speaks another wire format (or needs other methods), supply the codec yourself: `handle_with(filter, handler, decode, encode)` pairs a `MethodFilter` (unions work, e.g. `MethodFilter::POST.or(MethodFilter::PUT)`) with a decoder `Fn(RawRequest<'_>) -> Result<I, E>` (any `E: Into<HttpError>`, so a decoder can classify its refusal) over the raw request (path parameters, query, headers, body) and an encoder `Fn(F::Output) -> Response` (reuse `axum::Json` for JSON output); `consume_with(handler, decode)` takes a decoder `Fn(&Delivery) -> Result<I, DecodeError>` over the whole delivery. Errors keep flowing through `Into<HttpError>`: an `omnia_guest::Error` becomes the JSON `api::ErrorBody` (`{"error","message"}`) at its status, and `HttpError::with_body` carries a preformatted error body (e.g. an XML document) with its content type.

### Command-mode guests

`api::command` is the command-line mirror of `api::http` over the same handlers (`command` feature for the clap-backed parts): `parse::<App>(argv)` classifies argv into the grammar or one of clap's own responses (`Parsed::{App, Display, Usage}`), and `Command::new(&client, &metadata, format).call(handler, decode, render)` projects one verb — decode → `Client::call` → encode — onto a `Response { stdout, stderr, exit }`. `omnia_guest::command!(main)` binds an `async fn main() -> Response` as the `wasi:cli/run` export and writes the channels at that boundary (`IntoExit`; `Result<(), u8>` and `()` entries are accepted too). Omnia creates a fresh component instance for each command invocation.

```rust,ignore
use omnia_guest::api::command::{Command, Parsed, Response, parse};
use omnia_guest::api::{Client, Metadata};

omnia_guest::command!(main);

async fn main() -> Response {
    let app = match parse::<App>(wasip3::cli::environment::get_arguments()) {
        Parsed::App(app) => app,
        Parsed::Display(text) => return Response::success(text),
        Parsed::Usage(error) => return Response::usage(&error),
    };
    let client = Client::new("my-org", MyProvider);
    let metadata = Metadata::from_env("APP");
    let command = Command::new(&client, &metadata, app.format);
    match app.verb {
        Verb::Create(input) => command.call(create_item, || Ok(input), render_item).await,
    }
}
```

A success body is encoded in the selected `api::Format` (`Text` through the `render` closure, `Json` pretty-printed) onto stdout at exit 0. A decode or handler error is a `Failure` envelope on stderr at `Error::exit_code()` — `BadRequest` 1, `NotFound` 2, `ServerError` 3, `BadGateway` 4 — rendered as `error[<code>]: <message>` (plus `hint: …` when `.hints(..)` or `with_hint` attached one) or as flat JSON `{"error","message","exit-code","hint"?}`; `error` and `message` are the same `ErrorBody` HTTP emits. A clap usage error exits `USAGE_EXIT` (64), so exit 2 always means `NotFound`. `Metadata::from_env(prefix)` reads `<PREFIX>_REQUEST_ID` / `_CORRELATION_ID` / `_CAUSATION_ID`, minting a request id when absent as every transport does.

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

The crate provides an `Error` enum with four variants (`BadRequest`, `NotFound`, `ServerError`, `BadGateway`), each mapped to an HTTP status (`Error::status()`) and a process exit code (`Error::exit_code()`: 1 / 2 / 3 / 4), and helper macros for ergonomic error creation. Every transport reports a failure as the same `api::ErrorBody { error, message }` (`code()` and `description()`).

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
- `http` *(default)*: the axum-backed `api::http` routing, the `mcp` server, and the `HttpError` / `HttpResult` root re-exports. The outbound `HttpRequest` capability and `Error::status()` need no feature.
- `command`: the clap-backed parts of `api::command` (`parse`, `completions`, `Response::usage`, the `clap_complete::Shell` re-export, and the `clap::ValueEnum` derive on `api::Format`). The `Command` projector, `Response`, `Failure`, and `command!` need no feature.

Guests that do not use SQL, documents, or inbound HTTP can disable defaults to shrink wasm build time and size; a command-only guest is `default-features = false, features = ["command"]`.

## License

MIT OR Apache-2.0
