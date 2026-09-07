# CLI Command Example

Demonstrates a `wasi:cli/command` guest: clap argv, `Client::call` on
fn handlers, and `command::execute_wasi`.

## Quick Start

```bash
make build cli

# test
make run cli greet Ada
make run cli add 2 3 4
make run cli env
make run cli --help
make run cli -- bogus; echo $?   # 2
make run cli -- fail 42; echo $?  # 42
make run cli -- fail; echo $?     # 1
```

Or, more manually, for debugging:

```bash
# build the guest
cargo build --example cli-wasm --target wasm32-wasip2

# run the host (everything after `--` is the guest argv)
export RUST_LOG=info,opentelemetry_sdk=off
cargo run --example cli -- run ./target/wasm32-wasip2/debug/examples/cli_wasm.wasm -- greet Ada

# test
cargo run --example cli -- run ./target/wasm32-wasip2/debug/examples/cli_wasm.wasm -- greet Ada
...
```

