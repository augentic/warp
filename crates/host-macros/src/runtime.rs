//! # Runtime macro configuration and expansion
//!
//! Parses `runtime!({ ... })` and expands it into a complete runtime module.

mod codegen;
mod parse;

use proc_macro2::TokenStream;
use quote::quote;

use crate::runtime::codegen::Codegen;
pub use crate::runtime::parse::Config;

/// Generate the runtime module from a parsed [`Config`].
pub fn expand(config: &Config) -> TokenStream {
    let Codegen {
        mode,
        host_types,
        ctx_keys,
        backends_ty,
        backends_def,
        main_options,
        manifest,
        link_plugins,
    } = Codegen::from(config);

    let mode = mode.tokens();
    // A declared `locations:` list opts the deployment into the loader host —
    // worlds that do not import `omnia:plugins/loader` never see it — and
    // installs those locations. Interfaces-only deployments emit no plugin
    // path at all, so they build without `omnia`'s `plugin` feature.
    let plugins_host = link_plugins.then(|| {
        quote! {
            deployment.host::<omnia::WasiPlugins, B>()?;
        }
    });
    let extend_hook = link_plugins.then(|| {
        quote! {
            fn extend(runtime: &omnia::Runtime<B>) -> Result<()> {
                omnia::Plugins::install_declared(runtime)
            }
        }
    });
    let manifest = manifest
        .unwrap_or_else(|| quote! { omnia::ManifestSource::Inline(omnia::Manifest::new()) });

    quote! {
        mod runtime {
            // Every path resolves through the facade so an embedder's only
            // required dependency is `omnia` itself.
            use omnia::anyhow::Result;
            use omnia::futures::future;
            use omnia::Server;
            use omnia::tokio;
            use super::*;

            #backends_def

            /// This runtime's host wiring, generic over the bundle carrying
            /// its hosts' contexts: the compiled-in bundle under `main` and
            /// `run`, any other under `run_with`.
            pub struct Hooks;

            impl<B> omnia::Wiring<B> for Hooks
            where
                B: Clone + Send + Sync + 'static,
                #(B: omnia::Provides<#ctx_keys>,)*
            {
                fn link(deployment: &mut omnia::Deployment<omnia::StoreCtx<B>>) -> Result<()> {
                    #plugins_host
                    #(deployment.host::<#host_types, B>()?;)*
                    Ok(())
                }

                #extend_hook

                async fn serve(runtime: &omnia::Runtime<B>) -> Result<()> {
                    // Every host runs uniformly: capability hosts resolve
                    // immediately through `Server`'s no-op default, trigger
                    // servers loop until shutdown.
                    let servers: Vec<future::BoxFuture<'_, Result<()>>> = vec![
                        #(
                            Box::pin(#host_types.run(runtime)),
                        )*
                    ];
                    future::try_join_all(servers).await?;
                    Ok(())
                }
            }

            /// The deployment compiled in here (`config:` or the inline
            /// manifest keys; empty when neither is declared), for an
            /// embedder to overlay before `run_with`.
            pub fn manifest() -> omnia::ManifestSource {
                #manifest
            }

            /// Entry point: run the compiled-in deployment through this
            /// runtime's hosts and backends (raw argv passthrough for a
            /// command deployment compiled in here, otherwise the standard
            /// `run` grammar).
            #[tokio::main]
            pub async fn main() -> ::std::process::ExitCode {
                omnia::main::<#backends_ty, Hooks>(#main_options).await
            }

            /// Run one deployment through this runtime's hosts and backends,
            /// blocking until the guest completes.
            #[tokio::main]
            pub async fn run(builder: omnia::DeploymentBuilder) -> Result<omnia::ExitStatus> {
                let deployment = builder.mode(#mode).build::<omnia::StoreCtx<#backends_ty>>().await?;
                omnia::run::<#backends_ty, Hooks>(deployment).await
            }

            /// Run one deployment through this runtime's hosts over a bundle
            /// already in hand — nothing connects.
            pub async fn run_with<B>(
                builder: omnia::DeploymentBuilder, backends: B,
            ) -> Result<omnia::ExitStatus>
            where
                B: Clone + Send + Sync + 'static,
                Hooks: omnia::Wiring<B>,
            {
                let deployment = builder.mode(#mode).build::<omnia::StoreCtx<B>>().await?;
                omnia::run_with::<B, Hooks>(deployment, backends).await
            }
        }

        #[allow(unused_imports)]
        pub use runtime::{Hooks, main, manifest, run, run_with};
    }
}

// Unit tests by design: macro token expansion is pure.
#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    // Expand a `runtime!` config and pretty-print the output so snapshots are
    // readable and diffs are line-oriented.
    fn expand_pretty(input: proc_macro2::TokenStream) -> String {
        let config: Config = syn::parse2(input).expect("config parses");
        let file = syn::parse2::<syn::File>(expand(&config)).expect("expansion parses as a file");
        prettyplease::unparse(&file)
    }

    #[test]
    fn expand_server() {
        insta::assert_snapshot!(expand_pretty(quote!({
            hosts: {
                WasiHttp: HttpDefault,
                WasiOtel: OtelDefault,
                WasiKeyValue: KeyValueDefault,
            },
        })));
    }

    // A `Backend(options)` row lowers to `connect_with(options)`; rows
    // sharing that backend ride the same compiled-in connection.
    #[test]
    fn expand_connect_options() {
        insta::assert_snapshot!(expand_pretty(quote!({
            hosts: {
                WasiKeyValue: Filesystem(FilesystemOptions::at(".omnia/storage")),
                WasiBlobstore: Filesystem(FilesystemOptions::at(".omnia/storage")),
                WasiOtel: OtelDefault,
            },
        })));
    }

    // A backend shared by non-adjacent hosts must emit exactly one struct
    // field (interleaved duplicates defeat a consecutive-only dedup).
    #[test]
    fn expand_shared_backend() {
        insta::assert_snapshot!(expand_pretty(quote!({
            hosts: {
                WasiKeyValue: Redis,
                WasiOtel: OtelDefault,
                WasiMessaging: Redis,
            },
        })));
    }

    #[test]
    fn expand_command() {
        insta::assert_snapshot!(expand_pretty(quote!({
            mode: command,
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    #[test]
    fn expand_config_file() {
        insta::assert_snapshot!(expand_pretty(quote!({
            config: concat!(env!("CARGO_MANIFEST_DIR"), "/omnia.toml"),
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    // A `command: true` guest entry marks the command-mode target; the flag
    // expands to `.command()` on its `GuestEntry`.
    #[test]
    fn expand_command_flag() {
        insta::assert_snapshot!(expand_pretty(quote!({
            mode: command,
            guests: [
                { id: "app", source: "app.wasm", command: true },
                { id: "helper", source: "helper.wasm" },
            ],
        })));
    }

    // The composed deployment shape: static guests, mounts, and explicit
    // command routing.
    #[test]
    fn expand_deployment_keys() {
        insta::assert_snapshot!(expand_pretty(quote!({
            mode: command,
            guests: [
                { id: "specify", source: engine_component_path(), command: true },
                { id: "target:mock", source: mock_target_path() },
            ],
            mounts: [
                { name: "project", path: project_root(), writable: true },
                { name: "store", path: store_root(), writable: true },
            ],
            hosts: {
                WasiHttp: HttpDefault,
                WasiOtel: OtelDefault,
            }
        })));
    }

    // A bytes-valued `source:` (the `include_bytes!` embedding shape) passes
    // through to `GuestEntry::new` unchanged.
    #[test]
    fn expand_embedded_bytes() {
        insta::assert_snapshot!(expand_pretty(quote!({
            guests: [
                {
                    id: "specify",
                    source: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/specify.wasm")),
                },
            ],
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    // Guest-owned routes and the deployment-wide plugins block: every trigger
    // list expands to `route_*` builder calls on the owning `GuestEntry` (the
    // guest id is the implicit target), the `plugins:` block's `interfaces:`
    // list to `.link(...)` calls on the `Manifest` — and, with no
    // `locations:`, no loader host link — and patterns/interfaces are
    // arbitrary expressions.
    #[test]
    fn expand_inline_manifest() {
        insta::assert_snapshot!(expand_pretty(quote!({
            plugins: { interfaces: ["omnia:link/echo"] },
            guests: [
                {
                    id: "responder",
                    source: concat!(env!("CARGO_MANIFEST_DIR"), "/responder.wasm"),
                    routes: {
                        messaging: ["orders.>"],
                        websocket: ["chat.*"],
                    },
                },
                {
                    id: "router",
                    source: concat!(env!("CARGO_MANIFEST_DIR"), "/router.wasm"),
                    routes: {
                        http: ["/", concat!("/", "api")],
                    },
                },
            ],
            mounts: [
                { name: ".", path: concat!(env!("CARGO_MANIFEST_DIR"), "/workspace"), writable: true },
            ],
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    // The declarative locations grammar: each entry lowers into a
    // `Location` on the inline manifest, and the declared `locations:`
    // list links the loader host and makes the generated `Wiring::extend`
    // install them.
    #[test]
    fn expand_locations() {
        insta::assert_snapshot!(expand_pretty(quote!({
            plugins: {
                interfaces: ["emery:adapter/probe"],
                locations: [
                    { name: ".", path: project_root() },
                    { registry: "ghcr.io" },
                ],
            },
            guests: [
                { id: "engine", source: "engine.wasm" },
            ],
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    // A `plugins:` block without `locations:` is interfaces-only: no loader
    // host link, no `extend` hook, so the expansion never names a plugin
    // path and builds without `omnia`'s `plugin` feature.
    #[test]
    fn expand_plugins_without_locations() {
        insta::assert_snapshot!(expand_pretty(quote!({
            plugins: { interfaces: ["emery:adapter/probe"] },
            guests: [
                { id: "engine", source: "engine.wasm" },
            ],
        })));
    }

    // A bare `plugins: {}` beside `config:` is the config-file deployment's
    // opt-in: its locations live in the TOML's `[[location]]` entries, so
    // the loader host links and `extend` installs whatever they declare.
    #[test]
    fn expand_config_file_with_plugins() {
        insta::assert_snapshot!(expand_pretty(quote!({
            config: concat!(env!("CARGO_MANIFEST_DIR"), "/omnia.toml"),
            plugins: {},
            hosts: {
                WasiOtel: OtelDefault,
            },
        })));
    }

    // Locations are manifest data, so they conflict with `config:` like
    // every other inline key; the TOML declares them instead.
    #[test]
    fn locations_refused_beside_config() {
        let error = syn::parse2::<Config>(quote!({
            config: concat!(env!("CARGO_MANIFEST_DIR"), "/omnia.toml"),
            plugins: {
                locations: [{ registry: "ghcr.io" }],
            },
        }))
        .err()
        .expect("locations beside config must be refused");
        assert!(error.to_string().contains("mutually exclusive"), "{error}");
    }
}
