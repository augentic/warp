# Guest link (host-mediated dynamic linking)

Proves host-mediated dynamic linking: one guest reaches another through an interface the *host* satisfies at runtime, carried over in-process [wRPC](https://github.com/bytecodealliance/wrpc).

## What it shows

- `responder` ([`responder.rs`](responder.rs)) **exports** `omnia:link/echo`. It declares no trigger of its own, so it is reachable *only* via dispatch.
- `router` ([`router.rs`](router.rs)) **imports** `omnia:link/echo` and exposes `run(message)`. Its component does not satisfy the import.
- The [`runtime!` inline manifest](runtime.rs) — equivalently [`omnia.toml`](omnia.toml) for `--config`, or the programmatic [`Manifest`](dynamic.rs) — names `omnia:link/echo` in the deployment's `[link] interfaces`. The runtime core polyfills that import onto the shared linker and, at startup, wires the serve side of every dispatched interface.

When `router.run("hello")` calls the imported `echo("responder", "hello")`:

```mermaid
flowchart LR
  router["router.run<br/>(imports echo)"] -->|"echo(\"responder\", \"hello\")"| sel["FirstArgSelector<br/>target = responder"]
  sel --> guard["reject resources (§4.5)<br/>depth bound (§6.6)"]
  guard -->|"in-process wRPC"| resp["responder.echo<br/>(fresh instance per call)"]
  resp -->|"\"responder echoes: hello\""| router
```

The selector reads the leading argument (`"responder"`) to pick the target and forwards it through; the responder is instantiated **fresh per call** (instance-per-call) and discarded.

The interface also carries an async-typed dual, `echo-slow: async func`, called by the router's async-lifted `run-slow`. It rides the same dispatch, registered with `func_new_concurrent` instead of `func_new_async` (an async-typed import fails the sync registration's typecheck), and the responder parks on a `wasi:clocks` timer before answering — proving the round-trip against a callee that is genuinely pending.

The runtime core stays generic (Law 2): `[link] interfaces` and the selector operate on the opaque interface string `omnia:link/echo` and opaque guest ids — Omnia never parses the interface's meaning.

## Quick Start

This example deploys two guests from either a TOML or programmatic manifest, so build and run stay manual:

```bash
# build the guests
cargo build -p examples \
  --example guest-link-responder-wasm \
  --example guest-link-router-wasm \
  --example guest-link-extra-wasm \
  --target wasm32-wasip2

# run the host — the deployment is compiled in (runtime! inline manifest
# keys), so a bare `run` works from any directory
export RUST_LOG=info,opentelemetry_sdk=off
cargo run --example guest-link -- run

# or with an explicit manifest
cargo run --example guest-link -- run --config examples/guest-link/omnia.toml

# or construct the same manifest dynamically in Rust
cargo run --example guest-link-dynamic

# or grow the deployment after startup: register a third guest (`extra`,
# absent from the manifest) at run time and dispatch to it via the router
cargo run --example guest-link-register
```

This emits `target/wasm32-wasip2/debug/examples/guest_link_responder_wasm.wasm`, `guest_link_router_wasm.wasm`, and `guest_link_extra_wasm.wasm` (the underscored names the manifest and `register.rs` point at).

The dynamic host uses the same generated runtime wiring, but supplies the
deployment at runtime:

```rust
let manifest = Manifest::new()
    .link(["omnia:link/echo"])
    .guest(GuestEntry::new("responder", responder_wasm))
    .guest(GuestEntry::new("router", router_wasm));

host::run(DeploymentBuilder::new().manifest(manifest))?;
```

## Dynamic registration

`extra` ([`extra.rs`](extra.rs)) also exports `omnia:link/echo` but is absent
from the manifest: [`register.rs`](register.rs) admits it after startup with
`Runtime::register` (verify → load → pre-instantiate → serve → publish), and the
router reaches it through `run-to("extra", ...)` — the same host-mediated
dispatch as any static target. `Runtime::deregister` removes it again;
in-flight calls complete (instance-per-call).

```rust
let bytes = std::fs::read(extra_wasm)?; // verified by the install pipeline
runtime.register("extra", GuestArtifact::wasm(bytes)).await?;
```

`GuestArtifact::wasm` is the safe constructor: raw component wasm is validated
and compiled inside the sandbox. Its dual, `GuestArtifact::precompiled`, is
`unsafe` — a serialized `omnia compile` artifact is native code, so the call
site must attest the bytes came unmodified from a trusted build pipeline.

