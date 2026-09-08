# WASI Interfaces Reference

Every capability Omnia exposes to guests, its interface crate, the zero-config default backend that ships in-tree, and the production backends available in the [`omnia-backends`](https://github.com/augentic/omnia-backends) repository.

## Interface matrix

| Interface | Crate | Host type | Default backend | Production backends |
| --------- | ----- | --------- | --------------- | ------------------- |
| HTTP client/server | `wasi-http` | `WasiHttp` † | `HttpDefault` — hyper outbound client, axum inbound server (`HTTP_ADDR`, default `0.0.0.0:8080`) | — |
| Key-value storage | `wasi-keyvalue` | `WasiKeyValue` | `KeyValueDefault` — in-memory cache | `omnia-redis`, `omnia-nats` |
| Pub/sub messaging | `wasi-messaging` | `WasiMessaging` † | `MessagingDefault` — in-process broadcast | `omnia-kafka`, `omnia-nats` |
| Blob storage | `wasi-blobstore` | `WasiBlobstore` | `BlobstoreDefault` — in-memory | `omnia-azure-blob`, `omnia-mongodb`, `omnia-nats` |
| SQL + ORM | `wasi-sql` | `WasiSql` | `SqlDefault` — SQLite (`SQL_DATABASE`, default shared in-memory) | `omnia-postgres` |
| Document store | `wasi-docstore` | `WasiDocStore` | `DocStoreDefault` — in-memory | `omnia-azure-table` |
| Configuration | `wasi-config` | `WasiConfig` | `ConfigDefault` — host process environment snapshot | — |
| Secrets vault | `wasi-vault` | `WasiVault` | `VaultDefault` — in-memory lockers | `omnia-azure-vault` |
| Identity/OAuth | `wasi-identity` | `WasiIdentity` | `IdentityDefault` — OAuth2 client flow (`IDENTITY_CLIENT_ID`, `IDENTITY_CLIENT_SECRET`, `IDENTITY_TOKEN_URL`) | `omnia-azure-id` |
| Observability | `wasi-otel` | `WasiOtel` | `OtelDefault` — log-only, no export | `omnia-opentelemetry` |
| WebSockets | `wasi-websocket` | `WasiWebSocket` † | `WebSocketDefault` — tungstenite server (`WEBSOCKET_ADDR`, default `0.0.0.0:80`) | — |
| Model completions | `wasi-model` | `WasiModel` | `ModelDefault` — deterministic echo (text/json echo the prompt; schema formats error) | `omnia-genai`, `omnia-cursor` |

† Trigger host: runs a server and drives guest instances per inbound request. All other hosts only serve guest-initiated calls.

Notes:

- Package names on crates.io carry the `omnia-` prefix (`omnia-wasi-keyvalue`, and so on); directory names in `crates/` drop it.
- Most defaults are genuinely zero-config and in-memory. The exceptions: `HttpDefault` and `WebSocketDefault` bind real TCP ports, and `IdentityDefault` needs OAuth credentials. `ModelDefault` is a zero-config echo — it answers text/json completions with the prompt itself but rejects `format::schema`, so deployments bind a real backend and tests define inline canned backends.
- Each interface crate compiles to guest bindings on `wasm32` and the host implementation on native targets, so guests and hosts depend on the same crate name.

## Crate anatomy

```text
wasi-keyvalue/
├── src/
│   ├── lib.rs          # cfg-gated guest vs host
│   ├── guest.rs        # Guest-side bindings (wasm32)
│   └── host/           # Host-side implementation (native)
│       ├── mod.rs      # WasiKeyValue host type, Host/Server impls
│       ├── default_impl.rs   # KeyValueDefault backend
│       └── ...
└── wit/                # WIT interface definitions (+ deps/)
```

## Supporting crates

| Crate | Purpose |
| ----- | ------- |
| `omnia` | Runtime core: engine, CLI, deployment, registry, dispatch, telemetry |
| `omnia-guest` | Guest SDK: `Handler`, `Client`, `Context`, the HTTP and messaging routers, the command façade (`parse`, `Command`, `Response`, `command!`), errors with their status/exit map, ORM, and MCP. Features: `orm` and `http` (default), `command` |
| `omnia-guest-macros` | `#[instrument]` attribute |
| `omnia-host-macros` | `runtime!` macro (use via `omnia::runtime!`) |
