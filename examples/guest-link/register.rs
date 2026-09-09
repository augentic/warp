//! Dynamic guest registration: grow the deployment after startup.
//!
//! Boots the two-guest `guest-link` deployment, then registers a third guest
//! (`extra`, absent from the manifest) at run time via `Runtime::register`.
//! The static `router` reaches it through the same host-mediated link dispatch
//! as any static target — `run-to("extra", ...)` — proving serve-at-register
//! end to end. Build the guests first (see README.md), then:
//!
//!   cargo run --example guest-link-register

cfg_if::cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        use std::path::Path;

        use anyhow::{Context as _, Result, bail};
        use omnia::wasmtime::component::Val;
        use omnia::{
            DeploymentBuilder, GuestArtifact, GuestEntry, GuestId, Manifest, Runtime, StoreCtx,
        };

        #[tokio::main]
        async fn main() -> Result<()> {
            let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/wasm32-wasip2/debug/examples");

            let manifest = Manifest::new()
                .link(["omnia:link/echo"])
                .guest(GuestEntry::new(
                    "responder",
                    artifacts.join("guest_link_responder_wasm.wasm"),
                ))
                .guest(GuestEntry::new("router", artifacts.join("guest_link_router_wasm.wasm")));

            // Raw `.wasm` sources, so the safe `build` applies; a deployment of
            // trusted `omnia compile` output would call `unsafe build_trusted`.
            let deployment = DeploymentBuilder::new()
                .manifest(manifest)
                .build::<StoreCtx<()>>()
                .await
                .context("building deployment")?;
            // `assemble` also wires the host-mediated link serve side.
            let runtime = deployment.assemble(()).await?;

            // The extra guest is absent from the manifest. An install pipeline
            // verifies the bytes (digest, signature — deployment policy) before
            // handing them to the runtime; here the "install" is a file read.
            // Raw wasm is the safe constructor; `GuestArtifact::precompiled` is
            // `unsafe` because pre-compiled bytes are native code.
            let wasm = std::fs::read(artifacts.join("guest_link_extra_wasm.wasm")).context(
                "extra guest not built: cargo build -p examples --example \
                 guest-link-extra-wasm --target wasm32-wasip2",
            )?;
            runtime.register("extra", GuestArtifact::wasm(wasm)).await?;

            // The static router dispatches to the registered guest exactly as it
            // would to a static one.
            println!("{}", call_router(&runtime, "extra", "hello").await?);
            println!("{}", call_router(&runtime, "responder", "hello").await?);

            runtime.deregister(&GuestId::from("extra"))?;
            Ok(())
        }

        /// Instantiate the router fresh and drive `run-to(target, message)`.
        async fn call_router(runtime: &Runtime<()>, target: &str, message: &str) -> Result<String> {
            let guest = runtime
                .registry()
                .get(&GuestId::from("router"))
                .context("router guest is registered")?;
            let mut store = runtime.build_store(runtime.store());
            let instance = runtime
                .instantiate(guest.instance_pre(), &mut store)
                .await
                .context("instantiating router")?;
            let run_to =
                instance.get_func(&mut store, "run-to").context("router exports `run-to`")?;

            let mut results = vec![Val::Bool(false)];
            run_to
                .call_async(
                    &mut store,
                    &[Val::String(target.to_owned()), Val::String(message.to_owned())],
                    &mut results,
                )
                .await
                .map_err(anyhow::Error::from)
                .context("calling router.run-to")?;

            match results.into_iter().next() {
                Some(Val::String(echoed)) => Ok(echoed),
                other => bail!("router.run-to returned a non-string result: {other:?}"),
            }
        }
    } else {
        fn main() {}
    }
}
