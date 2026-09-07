# omnia-guest-macros

Procedural attributes for Omnia guests. The crate provides one attribute, `#[instrument]`, which wraps a function in an OpenTelemetry span and initializes the guest subscriber on entry. Guests reach it as `omnia_wasi_otel::instrument`. Handlers, routing, and WASI exports are ordinary Rust APIs in `omnia-guest`.

## Instrumentation

```rust,ignore
use omnia_guest_macros::instrument;

#[instrument]
fn handle() {
    // a span named "handle" is active for the duration of this body
}

#[instrument(name = "custom_span", level = Level::DEBUG)]
async fn process() {
    // async bodies are instrumented too
}
```

Accepted arguments:

- `name` -- overrides the span name (defaults to the function name)
- `level` -- sets the span level (e.g. `Level::DEBUG`; defaults to `INFO`)
