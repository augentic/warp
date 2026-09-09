# Multi-Guest Deployments

A single runtime can host many guests, route inbound traffic between them, preopen host directories into their sandboxes, and let one guest call another through the host. All of this is deployment configuration in a TOML manifest — no host or guest code changes.

Come here once a single-guest runtime works ([Composing a Runtime](composing-a-runtime.md)): passing a single `.wasm` path to `run` remains the zero-config shorthand, and the manifest takes over when you need more than one guest, routes, or mounts. This page is the canonical walk-through of the manifest and its concepts; the field-by-field schema is in [Configuration](../reference/configuration.md#deployment-manifest-omniatoml).

## The deployment manifest (`omnia.toml`)

Point the runtime at a manifest with `--config` (or the `OMNIA_CONFIG` environment variable):

```bash
cargo run --example http-routing -- run --config examples/http-routing/omnia.toml
```

A runtime can also compile in a default deployment with the `runtime!` macro — a manifest path via the `config:` field, or the manifest itself via the inline `link`/`plugin`/`guests`/`mounts` keys (each guest entry carries its own `routes`) — used only when the command line supplies no source (see [Composing a Runtime](composing-a-runtime.md#default-manifest-config)).

A manifest declares guests, mounts, routes, and (eventually) transports. Every field is optional except at least one `[[guest]]`. Paths resolve relative to the manifest's own directory.

```toml
[[guest]]
id = "api"                              # opaque identity; the runtime never parses it
source.path = "./guests/api.wasm"       # .wasm or pre-compiled .bin
routes.http = ["/"]

[[guest]]
id = "admin"
source.path = "./guests/admin.wasm"
routes.http = ["/admin"]
```

The full field reference lives in [Configuration](../reference/configuration.md#deployment-manifest-omniatoml).

## Programmatic manifests

Everything the TOML expresses can also be assembled in Rust: `omnia::Manifest` is the same schema as a value, with fluent setters for guests, mounts, link interfaces, and routes. Pass it to the deployment builder (or the `runtime!`-generated `run(builder)`) instead of a file path:

```rust,ignore
use omnia::{DeploymentBuilder, GuestEntry, Manifest};

let manifest = Manifest::new()
    .link(["omnia:link/audit"])
    .guest(GuestEntry::new("api", "./guests/api.wasm").route_http("/"))
    .guest(GuestEntry::new("admin", "./guests/admin.wasm").route_http("/admin"));

host::run(DeploymentBuilder::new().manifest(manifest))?;
```

`Manifest::from_config(path)?` loads a TOML file into the same value (resolving its relative paths against the file's directory), and `Manifest::from_wasm(path)` synthesizes the one-guest shorthand. Relative paths in a programmatic manifest resolve against the process working directory. The [`guest-link-dynamic`](../../examples/guest-link/dynamic.rs) example is a complete host built this way.

## Routing inbound traffic

Each guest declares the routes that target it, one optional list per trigger; the runtime aggregates them into per-trigger route tables at load:

- **`routes.http`** — path prefixes matched by longest prefix. One HTTP server fronts all guests.
- **`routes.messaging`** — topics matched by NATS-style pattern (`.`-separated tokens, `*` matches one token, `>` matches the rest).
- **`routes.websocket`** — same pattern syntax, for WebSocket routes.

If a trigger has no routes and exactly one guest exports its handler, that guest is the catch-all — so single-guest deployments need no routes at all.

The [`http-routing`](../../examples/http-routing/) example runs two HTTP guests behind `/a` and `/b` prefixes.

A messaging deployment works the same way: the host backend subscribes to topics (broker configuration such as `KAFKA_TOPICS`/`NATS_TOPICS`, or everything for the in-memory default), and each guest's route list picks which delivered messages it handles:

```toml
[[guest]]
id = "orders"
source.path = "./guests/orders.wasm"     # exports the messaging handler
routes.messaging = ["orders.>"]          # orders.created, orders.cancelled, ...

[[guest]]
id = "billing"
source.path = "./guests/billing.wasm"    # exports the messaging handler
routes.messaging = ["invoices.*"]        # exactly one token after `invoices.`
```

Each matched message instantiates a fresh instance of the routed guest, exactly like an HTTP request. Inside the guest, topic-to-handler matching stays exact — see [Messaging](messaging.md#handling-incoming-messages).

## Mounts: giving guests a workspace

Guests run in a sandbox with no filesystem access by default. A **mount** preopens a host directory into every guest's sandbox under a guest-visible name:

```toml
[[mount]]
name = "."          # what the guest sees in preopens.get-directories()
path = "../.."      # host path, relative to the manifest
writable = true     # omit for read-only (the default)
```

The equivalent on the command line (repeatable, layered over the manifest, last-wins per name):

```bash
cargo run --example cli -- run guest.wasm --mount path=workspace,name=.,writable
```

Guests discover mounts through `wasi:filesystem/preopens`:

```rust
let directories = preopens::get_directories();
let workspace = directories.iter().find_map(|(dir, name)| (name == ".").then_some(dir));
```

The [`model`](../../examples/model/) example lends a mounted workspace to a model backend this way.

## Guest-to-guest linking

One guest can import an interface that another guest exports, with the host mediating the call. The deployment names the interface in its `[link] interfaces`:

```toml
[link]
interfaces = ["omnia:link/echo"]

[[guest]]
id = "responder"
source.path = "./responder.wasm"        # exports omnia:link/echo

[[guest]]
id = "router"
source.path = "./router.wasm"           # imports omnia:link/echo
```

At startup, the runtime polyfills each dispatched interface onto the shared linker and dispatches calls to whichever guest exports it, over an in-process channel. The runtime sees only opaque interface strings and guest identities — no domain knowledge lives in the core (this is the glossary's [Law 2](../glossary.md#law-2)).

Notes:

- `[link] interfaces` is deployment-wide: the linker is shared, so a dispatched interface is wired for the whole deployment, and any guest importing it may call it. `--link <interface>` on the command line unions with the manifest's list. An exporter need not be loaded at startup — a guest registered later can serve the interface. A runtime built without the `link` feature refuses a non-empty list at startup.
- Nested dispatch depth is bounded by `MAX_DISPATCH_DEPTH` (default 8) to catch accidental recursion.
- Only the in-process transport is implemented; declaring `unix`, `nats`, or `quic` under `[transport]` is rejected at load.

The [`guest-link`](../../examples/guest-link/) example is a complete router/responder pair.

## How execution scales

All guests share one wasmtime engine and linker, and each is pre-instantiated once at startup. Every inbound request or dispatched call then instantiates a fresh instance in its own store, so guests never share state within or across requests. The pooling allocator (on by default) recycles instance slots to keep per-request cost low — tunables are listed in [Configuration](../reference/configuration.md#instance-pooling).
