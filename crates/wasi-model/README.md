# Omnia WASI Model

This crate provides the `omnia:model/completion` boundary for the Omnia runtime: the domain-agnostic contract a guest calls to have a prompt completed (`create: func(request) -> result<reply, error>`).

It owns only the boundary — the provider-shaped `request` (`system` / `messages` / `format` / `tools` / `grants` / `check`) and its `reply` / `error` envelope, the `WasiModelCtx` backend trait behind `create`, the per-completion session that carries tool calls and the guest's `check` between backend and guest, and the guest-side `Sections` prompt builder. It validates nothing about the answer: `format` steers the provider, and acceptance belongs to the guest, which sets `request.check` to judge each candidate (`Ok` accepts, `Err(text)` is the correction turn the backend appends verbatim before trying again). It knows nothing about which model, which provider, or any vendor SDK (Law 2). Real model backends (`omnia-genai`, `omnia-cursor`) live in the `omnia-backends` repo behind the same trait; tests define inline canned backends that return a fixed answer.

## Interface

Implements the `omnia:model` WIT interface (`completion`).

## Backend

- **Default**: `ModelDefault` (echo). It connects with zero configuration and answers every completion with its own prompt — the last message echoed as a string for `format::text`, wrapped as `{"echo": ...}` for `format::json` — so guest wiring runs deterministically with no live model. `format::schema` completions fail loud (no echo can conform to an arbitrary guest schema): bind a real backend, or define an inline canned `WasiModelCtx` in tests, which (unlike an echo) can satisfy a guest schema with a fixed answer. A `check` request gets exactly one: the echo has no second candidate, so a rejection is `budget-exhausted` carrying the guest's correction.

- **Production**: [`omnia-genai`](https://github.com/augentic/omnia-backends/tree/main/crates/genai) (provider APIs via the genai SDK), [`omnia-cursor`](https://github.com/augentic/omnia-backends/tree/main/crates/cursor) (bridge-managed Cursor agent) — a one-line swap in the host, guests untouched (see the [Production Backends guide](https://github.com/augentic/omnia/blob/main/docs/guides/production-backends.md)).

## Usage

Add this crate to your `Cargo.toml` and use it in your runtime configuration:

```rust,ignore
use omnia::runtime;
use omnia_wasi_model::ModelDefault;

omnia::runtime!({
    hosts: {
        WasiModel: ModelDefault,
    }
});
```

## License

MIT OR Apache-2.0
