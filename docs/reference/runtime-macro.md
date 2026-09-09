# `runtime!` Macro Reference

Every key the `omnia::runtime!` macro accepts, with exact semantics. The task-oriented walk-through — assembling your first runtime, choosing backends — is [Composing a Runtime](../guides/composing-a-runtime.md); manifests and multi-guest concepts are in [Multi-Guest Deployments](../guides/multi-guest-deployments.md).

## Key summary

| Key | Purpose | You need it when |
| --- | ------- | ---------------- |
| `hosts:` | The `Host: Backend` map — which WASI interfaces are linked and what implements them | Always (except a backend-less command runtime) |
| `mode:` | `server` (default) or `command` | Running jobs/CLIs instead of servers |
| `config:` | Compile in a default manifest *path* | You want `run` with no arguments to work |
| `link:`, `plugin:`, `guests:`, `mounts:` | Compile in a default manifest *value* (inline) | Same as `config:`, but self-contained — no TOML file at run time |

There is no key for raw argv passthrough: a command-mode runtime with a compiled-in deployment is a [direct command](#direct-commands-raw-argv-passthrough) automatically.

## `hosts:`

Each entry is a `Host: Backend` pair:

- The **host** type (`WasiHttp`, `WasiKeyValue`, ...) is the interface implementation from an `omnia-wasi-*` crate. It links the WASI functions into the wasmtime linker and, for trigger interfaces, runs a server.
- The **backend** type (`HttpDefault`, `KeyValueDefault`, or a production client such as `omnia_redis::Client`) is what the host delegates to. Every backend implements `omnia::Backend` and configures itself from environment variables at startup.

The macro generates a `Backends` bundle (one connected backend per entry, with one `omnia::Provides` accessor impl per row), the wiring that links each host and runs every host's `Server::run` (a no-op for capability hosts; the serve loop for trigger hosts), and a `#[tokio::main] main` that parses the CLI (`run` subcommand) and drives the runtime.

### Connect options

A backend name may carry a connect-options expression: `Host: Backend(options)` lowers to `Backend::connect_with(options)` instead of the env-sourced `Backend::connect()`, compiling configuration into the binary — a fixed storage root, a scripted test backend carrying state — rather than reading it from the environment at startup:

```rust
omnia::runtime!({
    hosts: {
        WasiKeyValue: Filesystem(FilesystemOptions::at(".omnia/storage")),
        WasiBlobstore: Filesystem(FilesystemOptions::at(".omnia/storage")),
        WasiOtel: OtelDefault,
    }
});
```

The expression's type is the backend's `Backend::ConnectOptions`. Rows sharing a backend type share one connection, so their options must be written identically on every row (or omitted on every row) — a mismatch is a compile error, as are empty parentheses (drop the `()` to connect from the environment).

## `mode:`

- **`mode: server`** (default) — the runtime stays up and serves requests. Trigger hosts (`WasiHttp`, `WasiMessaging`, `WasiWebSocket`) listen for traffic and instantiate a fresh guest instance per request.
- **`mode: command`** — the runtime drives the guest's `wasi:cli/run` export exactly once, then exits with the guest's status. Unlike server triggers, command mode applies no `GUEST_TIMEOUT_MS` wall-clock cap — to the run itself or to any link dispatch made along its call chain.

Command mode has two entry surfaces, chosen by whether the deployment is compiled in. With a compiled-in deployment (`config:` or inline manifest keys) the binary is a [direct command](#direct-commands-raw-argv-passthrough): no host CLI, argv passes to the guest verbatim. Without one, the standard `run … -- …` grammar applies — arguments after `--` are forwarded to the guest as its argv (`args[0]` is the program name, supplied by the runtime).

A backend-less command runtime is valid: `omnia::runtime!({ mode: command });`.

By default, command mode routes to the sole static guest exporting `wasi:cli/run`; a deployment with no exporter is inert and exits `0`. With several exporters, mark one guest entry `command: true` — see [Command routing](#command-routing-command-true).

## `config:` (default manifest path)

Compiles a default manifest path into the generated `main`, used only when the command line supplies no source — no positional wasm, no `--config`, no `OMNIA_CONFIG`:

```rust
omnia::runtime!({
    config: concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/omnia.toml"),
    hosts: {
        WasiHttp: HttpDefault,
    }
});
```

The value is any expression evaluating to a path. Anchoring it with `env!("CARGO_MANIFEST_DIR")` makes it absolute at compile time, so a bare `run` works from any working directory. Explicit sources always win; the compiled-in default is the lowest-precedence fallback.

`config:` and the inline manifest keys are mutually exclusive — a runtime compiles in a manifest path or a manifest value, not both.

## Inline manifest keys (`link:`, `plugin:`, `guests:`, `mounts:`)

The deployment `omnia.toml` expresses can also be written directly in the macro, mirroring the `omnia::Manifest` schema. The macro expands the keys to a `Manifest` value compiled into the generated `main` as the same lowest-precedence fallback as `config:`:

```rust
omnia::runtime!({
    link: {
        interfaces: ["omnia:link/echo"],     // host-mediated interfaces (deployment-wide)
    },
    plugin: {
        locations: [                         // optional: loader acquisition policy
            { name: ".", path: concat!(env!("CARGO_MANIFEST_DIR"), "/workspace") },
        ],
    },
    guests: [
        {
            id: "responder",
            source: concat!(env!("CARGO_MANIFEST_DIR"), "/guests/responder.wasm"),
            routes: {
                messaging: ["orders.>"],     // inbound routes targeting this guest
            },
        },
        {
            id: "router",
            source: concat!(env!("CARGO_MANIFEST_DIR"), "/guests/router.wasm"),
            routes: {
                http: ["/"],
                websocket: ["chat.*"],
            },
        },
    ],
    mounts: [
        { name: ".", path: concat!(env!("CARGO_MANIFEST_DIR"), "/workspace"), writable: true },
    ],
    hosts: {
        WasiHttp: HttpDefault,
    }
});
```

- Each value is any Rust expression evaluating to the field's type (strings for ids, interfaces, and route patterns; paths or embedded bytes for `source`, paths for mount `path`; a bool for `writable`, which defaults to `false`).
- Relative paths resolve against the process working directory at run time, so anchor them with `env!("CARGO_MANIFEST_DIR")` as with `config:`.
- Routes are declared per guest, on the target entry's `routes:` block — one pattern list per trigger (`http` prefixes, `messaging` topics, `websocket` routes), with the declaring guest as the implicit target. There is no top-level `routes:` key.
- A guest entry also accepts `command: true` (a literal bool), marking it as the command-mode target — see [Command routing](#command-routing-command-true).
- Host-mediated interfaces are declared once, deployment-wide, in the top-level `link:` block's `interfaces:` list — the linker is shared, so there is no per-guest form. `run --link` at the CLI unions with the compiled-in list. A bare `link: [...]` list is a compile error naming the block shape; a bare `link: {}` is a compile error because it would declare nothing. Declaring `link: { interfaces: [...] }` requires omnia's `link` feature; a runtime without it refuses a non-empty interface list at `Manifest::validate`. The removed `plugins:` key errors pointing at both `link:` and `plugin:`.
- Declaring a [`locations:` list](#plugin-locations-locations) also links the `omnia:plugins/loader` host capability (guest-requested plugin loading), which ships behind `omnia`'s non-default `plugin` feature; only guests whose world imports it can reach it. The list is the deployment's acquisition policy — the acquisition seam behind `loader.load`. A `link:`-only invocation never references the loader and builds without the feature, so a guest importing `omnia:plugins/loader` in such a deployment fails at instantiation; a bare `plugin: {}` beside `config:` does link it, over the TOML's `[[plugin.location]]` entries (without `config:` a bare block is a compile error, since it would declare nothing). Locations are manifest data (`[[plugin.location]]` in `omnia.toml`), so like every inline key they are mutually exclusive with `config:`; a config-file deployment declares them in the TOML. The two features are independent: neither block implies the other.

### Plugin locations (`locations:`)

The `locations:` list declares the deployment's acquisition roots. Each entry lowers to an `omnia::Location` on the compiled-in manifest, and the generated `Wiring::extend` hook installs them through `omnia::Plugins::install_declared`, each filling its location kind's slot on `Plugins`. Those paths live behind `omnia`'s `plugin` feature, so a deployment declaring `plugin: { locations: [...] }` enables it (`omnia = { version = "...", features = ["plugin"] }`); without it the expansion fails to compile naming `omnia::WasiPlugins`, and a config-file deployment with `[[plugin.location]]` entries is refused at startup.

```rust
omnia::runtime!({
    link: {
        interfaces: ["emery:adapter/probe"],
    },
    plugin: {
        locations: [
            { name: ".", path: project_root() },  // path loads resolve here
            { registry: "ghcr.io" },              // package references fetch here
        ],
    },
    guests: [
        { id: "engine", source: engine_path() },
    ],
    hosts: {
        WasiOtel: OtelDefault,
    }
});
```

An entry takes one of two shapes:

- **`{ name: ..., path: ... }`** — a named root for path loads. All path entries fold, in declaration order, into one `omnia::PathMounts`, whose directories open when the runtime assembles — a missing root fails startup rather than surfacing per load. A guest's `loader.load` names the location it resolves against; guests conventionally write paths relative to their mounts, so keep location names aligned with the mount names guests see.
- **`{ registry: ... }`** — the deployment's default registry endpoint for exact `namespace:name@version` package references, lowered to a cacheless `omnia::RegistryClient`. At most one entry; a load's own location may still override the endpoint per load.

Each load routes by its location kind — path loads to the path acquirer, registry loads to the registry acquirer — and a kind with no entry refuses typed. Routing is structural kind selection, never failure recovery.

The grammar's refusals are all compile-time, spanned to the offending key: an empty `locations:` list and a second `registry` entry are each rejected with a pointed diagnostic. The removed `cache:` key is refused with a migration hint: a store-backed registry acquirer (`RegistryClient::cached`) is installed by hand from a custom `Wiring::extend`.

Because locations are deployment data, a test can overlay them without touching the binary's declaration: `omnia_test::host::Deployment::from(runtime::manifest()).path_root(dir)` rewrites the `.` root and `runtime::run_with(...)` drives the same generated wiring over in-memory backends — see [Testing omnia code](../guides/testing-omnia-code.md).

### Embedding a guest (`source:` bytes)

`source:` also accepts component bytes, embedding the guest in the host binary — no adjacent `.wasm` file at run time:

```rust
omnia::runtime!({
    guests: [{
        id: "app",
        source: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/wasm32-wasip2/debug/examples/app_wasm.wasm",
        )),
    }],
    hosts: { /* ... */ }
});
```

Two things to know:

- **The guest artifact must exist when the host crate compiles** — `include_bytes!` reads it at build time, so the two-step build order (guest first, then host) becomes a hard requirement rather than a run-time one. This is why the repository's own examples stay path-based.
- **Embed raw `.wasm`, not `omnia compile` output.** Raw wasm is safe and JIT-compiles at startup (the `jit` feature is on by default). Embedded pre-compiled bytes are native code and are rejected by the safe build, same as pre-compiled paths; they require the programmatic `DeploymentBuilder`'s `unsafe build_trusted` — see the [security model](../security-model.md).

The [`guest-link`](../../examples/guest-link/runtime.rs) example is built this way; its [`omnia.toml`](../../examples/guest-link/omnia.toml) expresses the same deployment as a file for `--config`.

## Command routing (`command: true`)

**You need this when command mode should not rely on the sole-exporter default** — a deployment with several static guests exporting `wasi:cli/run`.

Marking one guest entry `command: true` routes command mode to that guest instead of the sole-static-exporter catch-all. The mark requires `mode: command`, and at most one guest may carry it — several are rejected at compile time (and, for a TOML manifest, at manifest validation). With several exporters the mark is also the safer form: a future guest accidentally exporting `wasi:cli/run` cannot flip the routing.

```rust
omnia::runtime!({
    mode: command,
    guests: [
        { id: "app", source: "app.wasm", command: true },
        { id: "helper", source: "helper.wasm" },
    ],
});
```

The same mark is available in `omnia.toml` (`command = true` on a `[[guest]]` entry — see [Configuration](configuration.md)) and programmatically (`omnia::GuestEntry::new(id, source).command()`). This leg is fail-closed: a marked identity nothing supplies, or one whose component does not export `wasi:cli/run`, fails the run instead of exiting inert.

`DeploymentBuilder::program_name` overrides the deployment name used for telemetry and prepended to guest argv as `argv[0]` (the default remains the manifest name).

## Direct commands (raw argv passthrough)

**Shipping a binary whose command line belongs entirely to the guest** — a product CLI where `mybin greet Ada` must work, not `mybin run guest.wasm -- greet Ada` — needs no key at all: a `mode: command` runtime with a compiled-in deployment (`config:` or inline manifest keys) is a *direct command*.

A direct command has no host `run` grammar: the binary's argv belongs to the guest. There is no `run` subcommand and no `--config`/`OMNIA_CONFIG`/positional-wasm override — the deployment compiled into the binary is the only source, by design. The program name used for telemetry and prepended to guest argv as `argv[0]` is the manifest name — the first `[[guest]]` id — unless overridden programmatically with `DeploymentBuilder::program_name`.

Two host log flags are reserved on this path: `--debug` and `--quiet`, anywhere in argv, are peeled before the guest sees them and select the host log preset (see [Host log flags](configuration.md#host-log-flags-direct-command-binaries)). Everything else passes through untouched.

A `mode: command` runtime *without* a compiled-in deployment keeps the `run` grammar byte-for-byte — with no other way to name the guest, the positional wasm path and `--config` remain the entry surface.

## Composing the keys

The [`cli-static`](../../examples/cli-static/runtime.rs) example composes the inline manifest keys into a complete direct command deployment with no handwritten `main`:

```rust
omnia::runtime!({
    mode: command,
    guests: [
        { id: "specify", source: engine_component_path(), command: true },
        { id: "target:mock", source: mock_target_path() },
    ],
    mounts: [
        { name: "project", path: project_root(), writable: true },
        { name: "store", path: store_root(), writable: true },
    ],
    hosts: {
        WasiHttp: HttpDefault,
        WasiOtel: OtelDefault,
        WasiModel: Cursor,
    }
});
```

## Generated items

The invocation expands to a private `runtime` module and re-exports five items from it:

| Item | Shape | Use |
| ---- | ----- | --- |
| `main` | `#[tokio::main] fn main() -> ExitCode` | The binary's entry point: connects the declared backends from the environment and drives the compiled-in deployment. |
| `run` | blocking `fn run(DeploymentBuilder) -> Result<ExitStatus>` | Build the builder, then mount the runtime in-process from a binary with its own argument surface. |
| `Hooks` | `pub struct Hooks` implementing `omnia::Wiring<B>` | The generated wiring — `link`, `extend`, `serve` — generic over any bundle `B` that `Provides` each declared host's context. |
| `manifest` | `fn manifest() -> ManifestSource` | The compiled-in deployment (`config:` path or inline keys), for a test to overlay. |
| `run_with` | `async fn run_with<B>(DeploymentBuilder, B) -> Result<ExitStatus>` | Build the builder, then drive the deployment through `Hooks` over a bundle already in hand; nothing connects. |

`main` and `run` use the generated `Backends` bundle. `Hooks`, `manifest`, and `run_with` exist so a test drives the *same* wiring the binary runs — over `omnia_test::host::Backends`, say — without a second `runtime!` declaration in the test tree.
