# Architecture

This document explains how Omnia is put together: the layering, the core abstractions, and the execution flow from CLI to guest invocation. It is background reading — for hands-on material, start with [Getting Started](getting-started.md) and the [how-to guides](README.md#how-to-guides).

For shared terminology (**runtime core**, **host-injected tools**, **Law 2**, and when "floor" means something else), see the [Glossary](glossary.md).

## Overview

Omnia is a thin, opinionated wrapper around [wasmtime](https://github.com/bytecodealliance/wasmtime) for running WASI components. It lets WebAssembly guests interact with external services (databases, message queues, models, and so on) through standardized WASI interfaces, while hosts swap backend implementations without changing guest code.

```text
┌─────────────────────────────────────────────────────────────────────┐
│                           Host Runtime                              │
│                                                                     │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌────────────┐     │
│  │  Backend   │  │  Backend   │  │  Backend   │  │  Backend   │     │
│  │  (Redis)   │  │  (Kafka)   │  │  (genai)   │  │ (in-memory)│     │
│  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘     │
│        │               │               │               │            │
│  ┌─────┴──────┐  ┌─────┴──────┐  ┌─────┴──────┐  ┌─────┴──────┐     │
│  │ wasi-kv    │  │ wasi-msg   │  │ wasi-model │  │ wasi-blob  │     │
│  │ (WASI API) │  │ (WASI API) │  │ (WASI API) │  │ (WASI API) │     │
│  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘     │
│        │               │               │               │            │
│        └───────────────┴───────┬───────┴───────────────┘            │
│                                │                                    │
│                         ┌──────┴──────┐                             │
│                         │    omnia    │                             │
│                         │ (wasmtime)  │                             │
│                         └──────┬──────┘                             │
│                                │                                    │
│   ┌────────────────────────────┴─────────────────────────────────┐  │
│   │                    WebAssembly Guests                        │  │
│   │        (Your application logic — one or many .wasm)          │  │
│   └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Guest/Host Architecture

Omnia follows the WebAssembly Component Model's guest/host pattern:

- **Guest**: Application code compiled to WebAssembly (`.wasm`), targeting `wasm32-wasip2` and using WASI Preview 3 bindings. The guest is portable and backend-agnostic.
- **Host**: The native runtime that loads and executes guests. It provides concrete implementations of WASI interfaces by connecting to actual backends.

This separation allows the same guest to run with different backends — swap the in-memory key-value store for Redis without changing application logic.

### Three-Layer Architecture

```text
┌─────────────────────────────────────────────────────────────────┐
│  Layer 3: Backends                                              │
│  Concrete connections to external services                      │
│  In-tree defaults (KeyValueDefault, SqlDefault, ...) and the    │
│  production crates in omnia-backends (redis, kafka, ...)         │
├─────────────────────────────────────────────────────────────────┤
│  Layer 2: WASI Interfaces (crates/wasi-*)                       │
│  Abstract service capabilities defined by WIT interfaces        │
│  Examples: wasi-keyvalue, wasi-messaging, wasi-model            │
├─────────────────────────────────────────────────────────────────┤
│  Layer 1: Composition root + live-runtime SDK                   │
│  omnia — assembly, lifecycle, optional-crate composition        │
│  omnia-core — wasmtime engine, registry, dispatch, traits       │
│  omnia-link — guest→guest linking (omnia `link` feature)        │
│  capability crates (omnia-plugin, …) target omnia-core          │
└─────────────────────────────────────────────────────────────────┘
```

Layers 1 and 2 form the **runtime core** — domain-agnostic infrastructure that routes opaque identities and satisfies typed effects. Within Layer 1, `omnia` is the composition root and `omnia-core` is the live-runtime SDK a capability crate targets. Which backend serves an interface is deployment configuration the core never parses (the glossary's **Law 2**).

## Crate Organization

### Composition root (`omnia`) and live-runtime SDK (`omnia-core`)

`omnia` is the composition root: it owns deployment assembly, process lifecycle, and composition of the optional crates. Embedders depend on it alone (`omnia::…` paths). A deployment's `Cargo.toml` never names `omnia-core`, `omnia-link`, `omnia-plugin`, or `omnia-cli`, and neither does any path the `runtime!` macro emits (it imports even its `Result` as `omnia::anyhow::Result`). Re-exports are `#[doc(inline)]` so the rendered documentation shows `omnia::…` paths too.

`omnia` provides:

- **Deployment pipeline**: `DeploymentBuilder` builds a `Deployment` from a `Manifest` (loaded from `omnia.toml`, synthesized from a single `.wasm`, or constructed programmatically)
- **Lifecycle**: `Wiring`, `Backends`, `Mode`; `run` / `run_with` take a built `Deployment`, call `Deployment::assemble`, then drive command mode or the trigger servers
- **Optional-crate composition**: `omnia-link` (guest→guest linking, behind the `link` feature — a `runtime!` invocation declaring `link: { interfaces: [...] }` requires it), `omnia-plugin` (the `omnia:plugins/loader` capability, behind the `plugin` feature — a `runtime!` invocation declaring `plugin: { locations: [...] }` requires it), `omnia-cli` (the `run` grammar, behind the `cli` feature), and the `runtime!` macro. The two features are independent: static guests may link; loaded guests may be host-only.

`omnia-core` is the live-runtime SDK a capability crate targets. Depend on it directly only when building another capability crate. It provides:

- **Runtime handle**: `Runtime<B>` over `StoreCtx<B>`, assembled from `RuntimeParts`; `Registry` holds pre-instantiated guests
- **Core traits**: `Host`, `Server`, `Backend`
- **Link seam**: the `LinkSeam` trait and `NoLinks` no-op the registry drives; guest→guest linking itself lives in `omnia-link` (`InProcessLinks`)
- **Host→guest dispatch**: `Dispatcher`, a direct call with no carrier
- **Telemetry**: `tracing` + OpenTelemetry bootstrap
- **Admission seam**: `Runtime::admit` and `Extensions`, which `omnia-plugin` uses to install acquisition policy from the `Wiring::extend` hook

`omnia-cli` is a leaf grammar crate: clap plus argv-precedence over paths and strings, with no `omnia-*` dependencies. `omnia` materializes a `RunPlan` into a `Manifest` and drives the runtime. `compile` (with the `jit` feature) also lives in `omnia`.

Key traits:

```rust
/// Implemented by all WASI hosts to link their functions into the shared linker.
pub trait Host<T>: Debug + Sync + Send {
    fn add_to_linker(linker: &mut Linker<T>) -> Result<()>;
}

/// Implemented by every WASI host; `run` is a no-op default that trigger
/// servers (HTTP, messaging, WebSocket) override with their serve loop.
pub trait Server<B>: Debug + Sync + Send {
    fn run(&self, state: &Runtime<B>) -> impl Future<Output = Result<()>>;
}

/// Implemented by backends for connection management.
pub trait Backend: Sized + Sync + Send {
    type ConnectOptions: FromEnv;
    fn connect() -> impl Future<Output = Result<Self>>;   // from environment
    fn connect_with(options: Self::ConnectOptions) -> impl Future<Output = Result<Self>>;
}
```

### WASI Interface Crates (`crates/wasi-*`)

Each interface crate provides guest bindings, a host implementation, and a default backend:

| Crate            | Interface        | Purpose                          | Default backend                           |
| ---------------- | ---------------- | -------------------------------- | ----------------------------------------- |
| `wasi-http`      | `wasi:http`      | HTTP client/server (trigger)     | `HttpDefault` — hyper client, axum server |
| `wasi-keyvalue`  | `wasi:keyvalue`  | Key-value storage                | `KeyValueDefault` — in-memory cache       |
| `wasi-messaging` | `wasi:messaging` | Pub/sub messaging (trigger)      | `MessagingDefault` — in-process broadcast |
| `wasi-blobstore` | `wasi:blobstore` | Object/blob storage              | `BlobstoreDefault` — in-memory            |
| `wasi-sql`       | `wasi:sql`       | SQL access + guest ORM           | `SqlDefault` — SQLite                     |
| `wasi-docstore`  | Custom           | JSON document store with filters | `DocStoreDefault` — in-memory             |
| `wasi-config`    | `wasi:config`    | Runtime configuration            | `ConfigDefault` — process environment     |
| `wasi-vault`     | Custom           | Secrets management               | `VaultDefault` — in-memory                |
| `wasi-identity`  | Custom           | Identity/OAuth tokens            | `IdentityDefault` — OAuth2 client flow    |
| `wasi-otel`      | Custom           | Guest OpenTelemetry export       | `OtelDefault` — log-only                  |
| `wasi-websocket` | Custom           | WebSocket connections (trigger)  | `WebSocketDefault` — tungstenite server   |
| `wasi-model`     | `omnia:model`    | Model completions with grants    | `ModelDefault` — deterministic echo       |

Conditional compilation lets one crate serve both sides — guests get bindings on `wasm32`, hosts get the implementation on native:

```rust
#[cfg(target_arch = "wasm32")]
mod guest;
#[cfg(not(target_arch = "wasm32"))]
mod host;
```

Each crate's `wit/` directory holds the [WIT](https://component-model.bytecodealliance.org/design/wit.html) interface definitions, with standard WASI dependencies vendored under `wit/deps/`.

### Guest SDK (`crates/omnia-guest`, `crates/guest-macros`)

`omnia-guest` defines transport-neutral `Handler`, `Client`, and `Context` plus three transport adapters under `omnia_guest::api`: the axum-backed HTTP router (`http`, behind the default `http` feature), the exact-topic messaging router (`messaging`), and the command façade (`command`: `parse` → `Command::call` → `Response`, with `command!` binding the `wasi:cli/run` export; clap-backed parts behind the `command` feature). Applications own their WASI exports and call the matching adapter. Failures share one vocabulary across transports: `Error` maps to an HTTP status and to an exit code (1/2/3/4; clap usage errors exit 64), and both HTTP and the command line emit the same `api::ErrorBody` discriminant. Every transport carrier (`x-request-id` headers, messaging metadata, `Metadata::from_env`) mints a request id when none arrives, and `Client::call` runs the handler in a span carrying it. ORM query builders (`orm` feature) and the MCP server module (`mcp::McpServer`, `mcp::router`) remain in `omnia-guest`; `guest-macros` provides only the independent `#[instrument]` tracing attribute.

### Host macro (`crates/host-macros`)

Provides the `runtime!` macro, re-exported as `omnia::runtime!`:

```rust
omnia::runtime!({
    mode: server,            // or `command`; server is the default
    hosts: {
        WasiHttp: HttpDefault,
        WasiOtel: OtelDefault,
        WasiKeyValue: Redis,
    }
});
```

The macro generates a `Backends` bundle (one connected backend per `Host: Backend` pair, with one uniform `omnia::Provides` impl per row wiring each backend into the library's generic `StoreView` blanket on `StoreCtx`), a `Wiring` implementation whose `link` runs on the `Deployment` before `assemble`, whose `extend` runs after, and whose `serve` runs every host (capability hosts resolve immediately through `Server`'s no-op default; trigger servers loop until shutdown), and a `main` that delegates to `omnia::main`. The runtime itself is always the library type `omnia::Runtime<Backends>` over `omnia::StoreCtx<Backends>` — the macro emits wiring, not a runtime.

## The Guest Registry

A deployment can hold many guests. All of them share one wasmtime `Engine` and one `Linker`; the `Registry` maps each opaque `GuestId` to a pre-instantiated `InstancePre`, so per-request instantiation is cheap. Three things hang off the registry:

- **Route tables** — per-trigger routing (each guest's `routes.http` by longest prefix, `routes.messaging`/`routes.websocket` by NATS-style pattern) selects which guest handles an inbound request.
- **Mounts** — `[[mount]]` entries preopen host directories into every guest sandbox (read-only unless marked writable).
- **Link seam** — the deployment-wide `[link] interfaces` list names interfaces the host polyfills onto the shared linker; calls dispatch to whichever guest exports the interface, over an in-process carrier, with nesting bounded by `MAX_DISPATCH_DEPTH`. The registry always holds a `LinkSeam`: `NoLinks` when the list is empty (every method a no-op), `InProcessLinks` (in `omnia-link`, reached only through omnia's `link` feature) otherwise.

Endpoints move through two stages inside the seam. `serve` runs *outside* the registry's lifecycle gate and writes only pending state; `publish`, `discard`, and `remove` run *under* the gate's write guard, so a guest's registry entry and its live endpoint change as one step. A call path never reads pending state and reads live state under the seam's own lock, not the gate: a call racing a deregister may complete against the departing instance, exactly as an in-flight invocation does.

All of this is declared in the `omnia.toml` manifest ([reference](reference/configuration.md#deployment-manifest-omniatoml)) or assembled programmatically with the `omnia::Manifest` fluent API; a bare `.wasm` path on the command line remains the zero-config single-guest case.

## Runtime Execution Flow

1. **CLI parsing** — the generated `main` delegates to `omnia::main`, which parses the `run` subcommand (`omnia-cli` decides the source over `--config` / `OMNIA_CONFIG` / positional `<wasm>` / compiled-in), materializes a `Manifest`, and appends CLI `--mount`/`--link` entries onto it.
2. **Build** — `DeploymentBuilder` validates the manifest, resolves mounts, loads guests, and returns a `Deployment` ready for host linking (`build` is the safe wasm path; `unsafe build_trusted` is the pre-compiled path).
3. **Assemble** — `run` connects backends, `Wiring::link` adds each host to the linker, `Deployment::assemble` builds the `Runtime` from `RuntimeParts` and wires host-mediated link servers, then `Wiring::extend` installs capability extensions (such as the plugin acquisition policy).
4. **Bootstrap** — starts epoch interruption and pool-metric sampling, then logs **`omnia ready`**.
5. **Drive** — command mode invokes the guest's `wasi:cli/run` once and exits with its status; server mode awaits every trigger server.
6. **Request handling** (server mode) — trigger hosts (`WasiHttp`, `WasiMessaging`, `WasiWebSocket`) accept requests, route to a guest, instantiate it in a fresh store, and return the response.

```text
CLI → Build → assemble → bootstrap → drive
                                       ├─ command mode → wasi:cli/run → ExitStatus
                                       └─ server mode  → trigger servers → per-request instantiate
```

### Isolation and pooling

Every invocation gets a **fresh instance in its own store** — no state survives between requests, and guests cannot observe each other except through host-mediated dispatch. To keep this cheap, the pooling instance allocator (on by default, `POOLING=true`) recycles instance slots; guest resource ceilings (`GUEST_TIMEOUT_MS` for server/dispatch, `MAX_MEMORY_BYTES`, `MAX_FUEL`) bound each invocation. See [Configuration](reference/configuration.md) for the tunables.

## Configuration

All backends and runtime options use environment variables, parsed via the `FromEnv` derive:

```rust
#[derive(Debug, Clone, FromEnv)]
pub struct ConnectOptions {
    #[env(from = "REDIS_URL", default = "redis://localhost:6379")]
    pub url: String,
}
```

The consolidated list is in [Configuration](reference/configuration.md); individual backend READMEs document service-specific variables.

## Directory Structure

```text
omnia/
├── crates/
│   ├── omnia/              # Composition root (assembly, lifecycle, optional crates, runtime!)
│   ├── omnia-core/         # Live-runtime SDK (engine, registry, dispatch, stores, telemetry)
│   ├── omnia-link/         # Guest→guest linking (InProcessLinks; re-exported by omnia behind `link`)
│   ├── omnia-guest/        # Guest SDK (Handler/Client/Context, HTTP/messaging/command adapters, errors, ORM, MCP)
│   ├── guest-macros/       # #[instrument] proc macro
│   ├── host-macros/        # runtime! proc-macro
│   ├── omnia-plugin/       # Plugins capability (loader host + acquisition; re-exported by omnia)
│   ├── omnia-cli/          # Leaf `run` grammar (selected by omnia's `cli` feature)
│   └── wasi-*/             # WASI interface implementations
│       ├── src/
│       │   ├── guest.rs    # Guest bindings (wasm32)
│       │   └── host/       # Host implementation + default backend (native)
│       └── wit/            # WIT interface definitions
├── examples/               # One guest + runtime pair per capability
│   └── <example>/
│       ├── guest.rs        # Guest code (→ .wasm)
│       └── runtime.rs      # Host code (→ native binary)
└── docs/                   # This documentation
```

Production backends live in the sibling [`omnia-backends`](https://github.com/augentic/omnia-backends) repository, one crate per service, each implementing `Backend` plus the relevant `WasiXxxCtx` traits.

## Extending Omnia

**Adding a WASI interface**: create `crates/wasi-<name>/` with the standard layout, define the WIT world in `wit/`, implement guest bindings in `src/guest.rs` and the host (including a zero-config default backend) in `src/host/`, and create an example. Unit-test deterministic logic (including the default backend against its `WasiXxxCtx`); do not add tests that compile or instantiate a WASM guest (see the [testing policy](guides/testing-policy.md)).

**Adding a backend**: create a crate (usually in the `omnia-backends` repo), implement `Backend` with a `FromEnv`-derived `ConnectOptions`, implement the `WasiXxxCtx` trait(s) for the interfaces it serves, and add `#[ignore]`-gated live tests. No runtime-core changes are required — backends plug in through the `runtime!` host map.

## Related Documentation

- [Getting Started](getting-started.md) — first build and run
- [Configuration reference](reference/configuration.md) — env vars and manifest format
- [wasmtime Component Model](https://docs.wasmtime.dev/api/wasmtime/component/)
- [WIT Format](https://component-model.bytecodealliance.org/design/wit.html)
- [examples/README.md](../examples/README.md) — running the examples
