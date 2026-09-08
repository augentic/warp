# Testing Policy

How the Omnia repository tests *itself*. The binding rules are also in the repository `AGENTS.md` (Testing policy); this page is the practical walk-through. For testing code *built on* omnia — a guest crate or an embedder — see [Testing Omnia-Based Code](testing-omnia-code.md), which covers the published `omnia-test` crate the suites below are built on.

## The tiers

- **End-to-end tests** are the primary tier for `wasi-*` host crates: a real guest component driven through omnia's own runtime against an inline scenario backend, pinning the whole boundary (guest bindings → linker → host binding → backend and back). The exemplar is `crates/wasi-model/tests/model.rs`.
- **Unit tests** cover deterministic logic wherever it lives: parsers, codecs, filter/type translation, route matching, macro token expansion, guest-side library code. If a behavior is a pure function no guest boundary reaches (e.g. `Format::parse` candidate extraction, which backends drive directly), it is a unit test next to that logic.
- **Live tests** (in the `omnia-backends` repo) are the acceptance tier for production backends: `#[ignore]`-gated, credential-gated, driving the backend's `WasiXxxCtx` against the real service.

Guest-instantiating tests exist **only** through the shared pipeline below. Do not compile, deserialize, or instantiate a WASM guest ad hoc inside an individual test.

## The e2e pipeline

One unpublished crate, patterned on wasmtime's `test-programs`, over the published `omnia-test`. **`crates/test-programs`** is both sides of the boundary:

- On `wasm32` it is the guest scenario programs, one `[[example]]` cdylib per scenario (`programs/<capability>/<scenario>.rs`), plus their shared helpers in `src/helpers.rs`. The example, path constant, and host test identity is `<capability>_<scenario>` (`model_echo_text`). Each program asserts what the guest observes across the boundary and traps on failure, and enters through `omnia_guest::command!(scenario)`.
- Natively it is the compiled artifacts. Its `build.rs` compiles every program to a `wasm32-wasip2` component through `omnia_test::build::Components` (into `OUT_DIR`, so plain `cargo make test` is self-contained), regenerates the `[[example]]` stanzas from the `programs/` tree, and writes `gen.rs`: one `pub const <NAME>: &str` artifact path per program plus a `foreach_<capability>!` macro, which `src/lib.rs` includes. The native side has no dependencies. The nested build compiles this same package for `wasm32` and so runs the script again; `Components` is a no-op under a `wasm32` target, which is what lets the guest package own its own fixture build.

The harness a suite drives the artifacts through is `omnia_test::host`, the same one a downstream consumer uses: `Deployment::new().guest(id, wasm).mounts(..).run_host::<H, B>(backends)` builds a one-shot `wasi:cli` command deployment and links the host under test beside `WasiOtel` (which `command!` imports; `run(backends, link)` takes a `link` closure for suites that link by hand), and `scratch()` mints a per-test workspace directory removed on drop.

A host crate's suite is one flat file per interface in its root `tests/` directory (`crates/wasi-model/tests/model.rs`). The file:

- invokes `test_programs::foreach_<capability>!();` so a guest program without a matching, identically named test fails to compile;
- defines its scenario backends inline next to the tests (see below);
- runs each guest with `Deployment::run_host` (via a small local `run_guest` wrapper supplying the `Has<Capability>` bundle and requiring `ExitStatus::SUCCESS`), then asserts any host-side effects (recorded requests, filesystem contents).

Assertions split by vantage point: the guest asserts what crosses the boundary to it (a panic traps and fails the host test); the host test asserts wire fidelity and side effects.

One group tests the guest SDK's own boundary rather than a `wasi-*` host: `programs/command/` holds `command!` guests built on the command façade (`command/exit_map`), and `crates/omnia-test/tests/command.rs` drives them with `Deployment::run` over the default bundle, asserting the `ExitStatus` the host observes for each verb — the exit map (`ok` 0, `bad` 1, `missing` 2, `upstream` 4) and `USAGE_EXIT` (64) for an unknown verb. These programs do not trap; the exit status *is* the behaviour under test, so the guest returns a `Response` and the host test reads `code_u8()`.

## Scenario backends

The bundle a suite runs over is `omnia_test::host::Backends`: the in-memory default for every host, deterministic (no environment read, no socket opened), with the model swappable for any `WasiModelCtx`. Most model scenarios script `ScriptedModel` — the answers, the tool calls and workspace steps each completion makes before answering, and the limits — and assert the recorded exchanges afterwards:

```rust,noplayground
let model = ScriptedModel::answering([json!("42")]).calling(0, [("lookup", "{}")]);
run_guest(test_programs::MODEL_TOOL_ROUNDTRIP, vec![], model.clone()).await;
model.assert_exhausted();
assert_eq!(model.exchanges(), [Exchange { tool: "lookup".into(), arguments: "{}".into(), outcome: Ok("42".into()) }]);
```

A behaviour a FIFO script cannot express — two tool calls in flight at once, a backend that ignores a hard failure — is a hand-written `WasiModelCtx` defined inline next to the test, with a comment saying why the script could not do it. The in-tree echo `ModelDefault` covers scenarios where the answer does not matter, or where its schema rejection is itself under test.

## Running

```bash
cargo make test                                   # `cargo nextest run --locked --all --all-features`
cargo test --doc --all-features --workspace       # doc tests
```

`cargo-nextest` must be installed with `--locked` (`cargo install --locked cargo-nextest`). The `wasm32-wasip2` target must be installed (`rust-toolchain.toml` pins it); `test-programs`'s build script needs it to compile the guest programs.

## Naming

A test name is the scenario (`set_then_get`), not a restated expectation (`set_then_get_round_trips`).
