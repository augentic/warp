# Composing a Runtime

The host runtime is a native binary that loads guests and provides their capabilities. This guide shows how to assemble one with the `runtime!` macro, in increasing order of sophistication: a basic server, command mode, a compiled-in deployment, and production backends. The exact semantics of every macro key live in the [`runtime!` Macro Reference](../reference/runtime-macro.md).

## The `runtime!` macro

Declare which WASI interfaces your deployment links (`hosts`) and which backend implements each one:

```rust
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

Each entry is a `Host: Backend` pair:

- The **host** type (`WasiHttp`, `WasiKeyValue`, ...) is the interface implementation from an `omnia-wasi-*` crate. It links the WASI functions into the wasmtime linker and, for trigger interfaces, runs a server.
- The **backend** type (`HttpDefault`, `KeyValueDefault`, or a production client such as `omnia_redis::Client`) is what the host delegates to. Every backend implements `omnia::Backend` and configures itself from environment variables at startup.

The macro generates:

- a `Backends` bundle holding one connected backend per entry,
- the wiring that links each host and runs every host's `Server::run` (a no-op for capability hosts; the serve loop for trigger hosts),
- a `#[tokio::main] main` that parses the CLI (`run` subcommand) and drives the runtime.

The result is a complete binary. Run it with:

```bash
cargo run -- run ./path/to/guest.wasm
```

## Server mode vs command mode

The optional `mode` key selects how the runtime drives guests:

- **`mode: server`** (the default) — the runtime stays up and serves requests. Trigger hosts (`WasiHttp`, `WasiMessaging`, `WasiWebSocket`) listen for traffic and instantiate a fresh guest instance per request.
- **`mode: command`** — the runtime drives the guest's `wasi:cli/run` export exactly once, then exits with the guest's status. Use this for jobs, CLIs, and agent tasks. Unlike server triggers, command mode applies no `GUEST_TIMEOUT_MS` wall-clock cap — to the run itself or to any link dispatch made along its call chain.

```rust
omnia::runtime!({
    mode: command,
    hosts: {
        WasiOtel: OtelDefault,
        WasiModel: ModelDefault,
    }
});
```

In command mode, arguments after `--` on the command line are forwarded to the guest as its argv:

```bash
cargo run --example cli -- run ./target/wasm32-wasip2/debug/examples/cli_wasm.wasm -- greet omnia
```

A backend-less command runtime is valid too: `omnia::runtime!({ mode: command });`.

By default, command mode routes to the sole static guest exporting `wasi:cli/run`. With several exporters, mark one guest entry `command: true` — see [Command routing](../reference/runtime-macro.md#command-routing-command-true) in the macro reference.

## Default manifest (`config:`)

The optional `config` key compiles a default manifest path into the generated `main`, used only when the command line supplies no source — no positional wasm, no `--config`, no `OMNIA_CONFIG`:

```rust
omnia::runtime!({
    config: concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/omnia.toml"),
    hosts: {
        WasiHttp: HttpDefault,
    }
});
```

The value is any expression evaluating to a path. Anchoring it with `env!("CARGO_MANIFEST_DIR")` makes it absolute at compile time, so a bare `run` works from any working directory:

```bash
cargo run -- run
```

Explicit sources always win; the compiled-in default is the lowest-precedence fallback.

The manifest itself — guests, routes, mounts, link interfaces — is covered in [Multi-Guest Deployments](multi-guest-deployments.md). A manifest can also be written inline in the macro instead of a TOML file; see [Inline manifest keys](../reference/runtime-macro.md#inline-manifest-keys-link-plugin-guests-mounts).

## Choosing backends

Every WASI interface ships with a default backend that needs no external service, so a development runtime works out of the box. Swapping to production is a one-line change per interface — the guest `.wasm` is untouched:

```rust
// Development
WasiKeyValue: KeyValueDefault,   // in-memory cache

// Production
WasiKeyValue: Redis,             // omnia_redis::Client from the omnia-backends repo
```

See [WASI Interfaces](../reference/wasi-interfaces.md) for the full default/production matrix and [Production Backends](production-backends.md) for wiring instructions.

## Backend configuration

Backends read their configuration from environment variables when the runtime starts, via the `FromEnv` trait:

```rust
#[derive(Debug, Clone, FromEnv)]
pub struct ConnectOptions {
    #[env(from = "REDIS_URL", default = "redis://localhost:6379")]
    pub url: String,
}
```

Runtime-wide settings (guest timeout, memory limits, instance pooling) are environment variables as well — see [Configuration](../reference/configuration.md).

## Observability and readiness

- The runtime configures `tracing` and OpenTelemetry at startup. Set `RUST_LOG=info` to see startup logs; set `OTEL_GRPC_URL` to export traces and metrics to an OTLP collector.
- Once bootstrap completes, the runtime logs **`omnia ready`** at `info` level (including the mode and guest count). Orchestrators can watch for this line to detect readiness.

## Advanced deployment keys

Most runtimes never need these. Each solves one specific deployment shape — reach for them when the situation matches, and see the [`runtime!` Macro Reference](../reference/runtime-macro.md) for exact semantics:

| Key | Reach for it when |
| --- | ----------------- |
| [`link:`/`plugin:`/`guests:`/`mounts:`](../reference/runtime-macro.md#inline-manifest-keys-link-plugin-guests-mounts) | You want the deployment compiled into the binary instead of a TOML file — including embedding the guest bytes themselves. |

Shipping a product CLI whose argv belongs entirely to the guest needs no key: a command-mode runtime with a compiled-in deployment is a [direct command](../reference/runtime-macro.md#direct-commands-raw-argv-passthrough) — no host `run` grammar at all.

The [`cli-static`](../../examples/cli-static/runtime.rs) example composes the inline manifest keys into a complete direct command deployment.

## Hand-written runtimes (advanced)

The macro covers most deployments. If you need a custom entry point — extra CLI flags, non-standard startup order, embedding the runtime in a larger process — supply the deployment yourself through the macro-generated `run(builder)`: build an `omnia::Manifest` (`Manifest::from_config(path)?` for a TOML file, `Manifest::from_wasm(path)` for the one-guest shorthand, or `Manifest::new()` with the fluent `guest`/`mounts`/`link`/`route_*` setters) and pass it via `omnia::DeploymentBuilder::new().manifest(manifest)`:

```rust,ignore
let manifest = Manifest::new()
    .link(["omnia:link/echo"])
    .guest(GuestEntry::new("responder", responder_wasm))
    .guest(GuestEntry::new("router", router_wasm));

host::run(DeploymentBuilder::new().manifest(manifest))?;
```

The [`guest-link-dynamic`](../../examples/guest-link/dynamic.rs) example is a complete host built this way. For still deeper control, implement the `omnia::Wiring` trait yourself, build a `Deployment`, and call `omnia::run(deployment)`, or `deployment.assemble(backends)` to obtain an `omnia::Runtime<B>`. The [`crates/omnia` README](../../crates/omnia/README.md) lists the public API surface; [Architecture](../Architecture.md) explains how the pieces fit together.

One case that requires this today: the generated `main` handles only the `run` subcommand. To expose ahead-of-time compilation (`compile`, available with the default `jit` feature), call `omnia::compile` from your own `main`. Parsing the standard grammar yourself with `omnia::Cli` requires the default `cli` feature (it re-exports the `omnia-cli` crate); `omnia::compile` does not.
