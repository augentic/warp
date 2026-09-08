# Direct Command Example

Demonstrates a compiled-in command deployment: a static guest declared
inline in the `runtime!` invocation. The static guest is the sole
`wasi:cli/run` exporter, so command mode routes to it with no
configuration.

Because the deployment is compiled in, command mode makes the binary a
**direct command** with no host CLI: there is no `run` subcommand and no
`--config`/`OMNIA_CONFIG`/positional-wasm override — every argument is
forwarded to the guest verbatim.

## Quick Start

```bash
# build the guest this deployment compiles in
cargo build --example cli-wasm --target wasm32-wasip2

# run the host: argv passes straight to the guest (no `run`, no `--`)
export RUST_LOG=info,opentelemetry_sdk=off
cargo run --example cli-static -- greet Ada
cargo run --example cli-static -- add 2 40
cargo run --example cli-static -- fail not-found; echo $?  # 2
```

The `--` above is cargo's own separator; the guest receives `greet Ada`
directly. Compare `examples/cli`, where the same guest runs through the
standard `run` grammar instead.
