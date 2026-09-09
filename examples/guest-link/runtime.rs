//! Host-mediated dynamic linking example runtime.
//!
//! Two guests are compiled in via the `runtime!` macro's inline manifest keys
//! (the Rust equivalent of `omnia.toml`): `responder` (exports
//! `omnia:link/echo`) and `router` (imports it, exports `run`). The router's
//! import is unsatisfied by its own component — the deployment names the
//! interface in its `link:` list, so the host polyfills it on the shared
//! linker and, at bootstrap, wires the serve side of every dispatched
//! interface (`omnia::serve_links`, run by `Deployment::assemble`), so a dispatched
//! call always finds the responder's in-process wRPC server.
//!
//! The router exports a plain `run` rather than an HTTP/messaging trigger;
//! running this binary starts the host and wires the link. See `README.md`.

cfg_if::cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        use omnia_wasi_http::{WasiHttp, HttpDefault};
        use omnia_wasi_otel::{WasiOtel, OtelDefault};

        omnia::runtime!({
            link: { interfaces: ["omnia:link/echo"] },
            guests: [
                {
                    id: "responder",
                    source: concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../target/wasm32-wasip2/debug/examples/guest_link_responder_wasm.wasm",
                    ),
                },
                {
                    id: "router",
                    source: concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../target/wasm32-wasip2/debug/examples/guest_link_router_wasm.wasm",
                    ),
                },
            ],
            hosts: {
                WasiHttp: HttpDefault,
                WasiOtel: OtelDefault,
            }
        });
    } else {
        fn main() {}
    }
}
