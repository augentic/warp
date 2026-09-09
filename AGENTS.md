# Agents

## Cursor Cloud specific instructions

### Overview

Omnia is a Rust monorepo (22 workspace crates + `examples`) providing a lightweight WASM (WASI) component runtime. Embedders depend on the `omnia` composition root, which owns deployment assembly and process lifecycle, re-exports the `omnia-core` live-runtime SDK, the `omnia-link` linking crate, the `omnia-plugin` capability crate, the `omnia-cli` leaf grammar crate (behind the `cli` feature), and the `runtime!` macro under one root; a deployment never depends on `omnia-core`, `omnia-link`, `omnia-plugin`, or `omnia-cli` directly, and code or docs that would require it are a bug. All WASI interfaces ship with in-memory defaults—no external services (Redis, NATS, Kafka, etc.) are needed for building, testing, or running examples.

Terminology (**runtime core**, **host-side**, **host-injected tools**, etc.) is defined in [docs/glossary.md](docs/glossary.md).

### Key commands

| Task         | Command                                                       |
| ------------ | ------------------------------------------------------------- |
| Build        | `cargo build --all-features`                                  |
| Lint         | `cargo clippy --all-features`                                 |
| Format check | `cargo +nightly fmt --all --check`                            |
| Format fix   | `cargo +nightly fmt --all`                                    |
| Test         | `cargo make test` (`cargo nextest run --all --all-features`)  |
| Doc tests    | `cargo test --doc --all-features --workspace`                 |
| Task runner  | `cargo make <task>` (see `Makefile.toml` for available tasks) |

### Running examples

Examples follow a two-step pattern: build the WASM guest, then run the native host runtime.

```
cargo build --example <name>-wasm --target wasm32-wasip2
cargo run --example <name> -- run ./target/wasm32-wasip2/debug/examples/<name>_wasm.wasm
```

For the HTTP example, the server listens on `localhost:8080`.

### Testing policy

The practical walk-through is [docs/guides/testing-policy.md](docs/guides/testing-policy.md). In short:

- **End-to-end tests are the primary tier for `wasi-*` host crates**: a real guest component from `crates/test-programs` (compiled by that crate's own build script) driven through omnia's own runtime (`omnia_test::host`) against an inline scenario backend, in one flat file per interface in the host crate's root `tests/` directory. Exemplar: `crates/wasi-model/tests/model.rs`, which invokes `test_programs::foreach_model!` so every guest program must have a matching test.
- **Unit tests for deterministic logic, wherever it lives**: parsers, codecs, filter/type translation, route matching, macro token expansion, guest-side library code. If a behavior is a pure function no guest boundary reaches, it is a unit test next to that logic.
- **Guest-instantiating tests exist only through the `test-programs` pipeline.** Never compile, deserialize, or instantiate a WASM guest ad hoc inside an individual test.
- **Production backends** (the `omnia-backends` repo) are accepted by `#[ignore]`-gated live tests against the real service, not by mapping unit tests alone.
- **Names identify, comments explain.** A test name is the scenario (`set_then_get`), not a restated expectation (`set_then_get_round_trips`).

### Gotchas

- `cargo-nextest` must be installed with `--locked` (`cargo install --locked cargo-nextest`); without it the build fails.
- Formatting uses `cargo +nightly fmt`, not stable rustfmt (the nightly toolchain must be installed).
- The `rust-toolchain.toml` pins the stable channel and auto-installs the `wasm32-wasip2` target plus `clippy`, `rust-src`, and `rustfmt` components.
- `edition = "2024"` and `rust-version = "1.95"` are workspace settings; ensure the stable toolchain is at least 1.95.
- Guest WASM examples compile to `wasm32-wasip2`; the binary name uses underscores (e.g., `http_wasm.wasm` not `http-wasm.wasm`).

### Code comments

Golden rule: do not document what is self-evident in code. Note, however, that the workspace lints (`missing_docs` plus clippy `pedantic`/`missing_errors_doc`, all enforced via `-D warnings` in `cargo make lint`) require a doc comment on every public item and an `# Errors` section on every public fallible function. Within that constraint:

- Keep public-item docs to a concise one-line summary; do not pad them by restating the signature, types, or fragile cross-references that a glance at the code already shows.
- Do not attach doc-comment labels to `impl` blocks (for example `From` conversions) — impl blocks need no docs, so a `/// X to Y mapping` line is pure noise.
- Inline comments (`//`) are never linted: add them only to explain *why* (security, performance, non-local control flow), never to narrate *what* the next line does.
- Trim redundant secondary sentences from multi-line docs, keeping the summary line the lint requires.
- The `examples` crate does not inherit the workspace lints, so prefer no doc comment over one that merely echoes a handler's name.
