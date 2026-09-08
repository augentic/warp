# Omnia: Lightweight WebAssembly Runtime

Omnia is a lightweight, secure runtime for WebAssembly (WASI) components. It provides a thin, ergonomic wrapper around [wasmtime](https://github.com/bytecodealliance/wasmtime) to easily integrate host-based services like HTTP, messaging, key-value stores, and model completions into your WASI applications.

While it can be used standalone, Omnia is primarily designed to be the runtime for **Augentic's Agent Skills**. It ensures that agent-generated code runs in a safe, sandboxed environment while still having controlled access to necessary infrastructure.

## Why Omnia?

- **Secure by Default**: All guest code runs in a strict WebAssembly sandbox. Capabilities (network, filesystem, model access) are explicitly granted.
- **Batteries Included**: Built-in support for common WASI interfaces — HTTP, key-value, messaging, SQL, blob and document storage, secrets, identity, WebSockets, observability, and model completions — each with a zero-config default backend.
- **Developer Friendly**: Typed explicit guest APIs and the `runtime!` host macro keep boundaries visible without repetitive transport glue.
- **Pluggable Architecture**: Swap backend implementations (e.g. in-memory to Redis) without changing or recompiling guest code. Production backends live in the sibling [`omnia-backends`](https://github.com/augentic/omnia-backends) repository.
- **Multi-Guest Deployments**: One runtime can host many guests with declarative routing, workspace mounts, and host-mediated guest-to-guest linking, all driven by a TOML manifest.

## Quick start

Build a guest and run it with an example host runtime:

```bash
cargo build --example http-wasm --target wasm32-wasip2
RUST_LOG=info cargo run --example http -- run ./target/wasm32-wasip2/debug/examples/http_wasm.wasm
```

A host runtime is a single macro invocation — each entry pairs a WASI interface with the backend that implements it:

```rust,ignore
use omnia_wasi_http::{HttpDefault, WasiHttp};
use omnia_wasi_keyvalue::{KeyValueDefault, WasiKeyValue};
use omnia_wasi_otel::{OtelDefault, WasiOtel};

omnia::runtime!({
    hosts: {
        WasiHttp: HttpDefault,
        WasiOtel: OtelDefault,
        WasiKeyValue: KeyValueDefault,
    }
});
```

## Documentation

**[Start with the documentation index](docs/README.md)** — a graduated path from first run to full deployments:

- [Getting Started](docs/getting-started.md) — first build and run, ~10 minutes
- How-to guides — [writing guests](docs/guides/writing-guests.md), [composing a runtime](docs/guides/composing-a-runtime.md), [multi-guest deployments](docs/guides/multi-guest-deployments.md), capability deep dives ([SQL](docs/guides/sql-and-orm.md), [documents](docs/guides/document-store.md), [messaging](docs/guides/messaging.md), [model/MCP](docs/guides/model-completions.md)), [production backends](docs/guides/production-backends.md), [deployment](docs/guides/deployment.md), [tuning](docs/guides/performance-tuning.md)
- [Architecture](docs/Architecture.md), [Security Model](docs/security-model.md), and [Glossary](docs/glossary.md)
- Reference — [WASI interfaces](docs/reference/wasi-interfaces.md), [model interface](docs/reference/model.md), [CLI](docs/reference/cli.md), [configuration](docs/reference/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)

The [`examples/`](examples/README.md) directory contains a complete working guest + runtime pair for every interface.

## Crates

| Crate                                           | Description                                                                                |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------ |
| [`omnia`](crates/omnia)                         | Composition root — assembly, lifecycle, optional-crate composition, and `runtime!` |
| [`omnia-core`](crates/omnia-core)               | Live-runtime SDK — wasmtime engine, registry, dispatch, stores, and telemetry |
| [`omnia-plugin`](crates/omnia-plugin)           | `omnia:plugins/loader` capability — loader host, acquisition policies, and plugin stores; selected by omnia's `plugin` feature |
| [`omnia-cli`](crates/omnia-cli)                 | Leaf `run` grammar — clap plus argv-precedence; selected by omnia's `cli` feature |
| [`omnia-guest`](crates/omnia-guest)             | Guest SDK — `Handler`/`Client`/`Context`, HTTP/messaging routers and the command façade, errors, ORM, and MCP support |
| [`omnia-guest-macros`](crates/guest-macros)     | `#[instrument]` observability attribute for guests                                         |
| [`omnia-host-macros`](crates/host-macros)       | `runtime!` proc-macro for host runtime generation                                          |
| [`omnia-wasi-blobstore`](crates/wasi-blobstore) | wasi:blobstore host and guest bindings                                                     |
| [`omnia-wasi-config`](crates/wasi-config)       | wasi:config host and guest bindings                                                        |
| [`omnia-wasi-docstore`](crates/wasi-docstore)   | Document-store host and guest bindings                                                     |
| [`omnia-wasi-http`](crates/wasi-http)           | wasi:http host and guest bindings                                                          |
| [`omnia-wasi-identity`](crates/wasi-identity)   | Identity/OAuth host and guest bindings                                                     |
| [`omnia-wasi-keyvalue`](crates/wasi-keyvalue)   | wasi:keyvalue host and guest bindings                                                      |
| [`omnia-wasi-messaging`](crates/wasi-messaging) | Messaging host and guest bindings                                                          |
| [`omnia-wasi-model`](crates/wasi-model)         | omnia:model completion host and guest bindings                                             |
| [`omnia-wasi-otel`](crates/wasi-otel)           | OpenTelemetry host and guest bindings                                                      |
| [`omnia-wasi-sql`](crates/wasi-sql)             | wasi:sql host and guest bindings, with guest ORM                                           |
| [`omnia-wasi-vault`](crates/wasi-vault)         | Secrets-vault host and guest bindings                                                      |
| [`omnia-wasi-websocket`](crates/wasi-websocket) | WebSocket host and guest bindings                                                          |

## License

MIT OR Apache-2.0
