//! Host-mediated linking with a programmatically assembled deployment manifest.

cfg_if::cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        use std::path::Path;

        use omnia::{DeploymentBuilder, GuestEntry, Manifest};
        use omnia_wasi_http::{HttpDefault, WasiHttp};
        use omnia_wasi_otel::{OtelDefault, WasiOtel};

        mod host {
            use super::*;

            omnia::runtime!({
                hosts: {
                    WasiHttp: HttpDefault,
                    WasiOtel: OtelDefault,
                }
            });
        }

        fn main() -> anyhow::Result<()> {
            let artifacts =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/wasm32-wasip2/debug/examples");
            let manifest = Manifest::new()
                .link(["omnia:link/echo"])
                .guest(GuestEntry::new("responder", artifacts.join("guest_link_responder_wasm.wasm")))
                .guest(GuestEntry::new("router", artifacts.join("guest_link_router_wasm.wasm")));

            host::run(DeploymentBuilder::new().manifest(manifest))?;
            Ok(())
        }
    } else {
        fn main() {}
    }
}
