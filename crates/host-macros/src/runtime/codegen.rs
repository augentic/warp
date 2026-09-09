//! # Codegen for the runtime macro.
//!
//! Generates the token stream fragments required to expand the runtime macro.

use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::{Expr, Ident, Path};

use crate::runtime::parse::{Config, HostEntry, LocationSpec, ManifestSpec, Mode};

// Token fragments needed to expand the runtime macro.
pub struct Codegen {
    pub mode: Mode,
    pub host_types: Vec<Path>,
    /// The `Provides` keys the generic `Hooks` impl bounds its bundle on,
    /// one per `hosts:` row.
    pub ctx_keys: Vec<TokenStream>,
    pub backends_ty: TokenStream,
    pub backends_def: TokenStream,
    pub main_options: TokenStream,
    /// The compiled-in `omnia::ManifestSource`; absent when the invocation
    /// declares neither `config:` nor inline manifest keys.
    pub manifest: Option<TokenStream>,
    /// Whether to link the `omnia::WasiPlugins` loader host and install the
    /// declared locations — a `locations:` list means the deployment opted
    /// into the loader capability (and into `omnia`'s `plugin` feature).
    pub link_loader: bool,
}

impl From<&Config> for Codegen {
    fn from(config: &Config) -> Self {
        let host_entries = &config.host_entries;
        let host_types: Vec<Path> = host_entries.iter().map(|entry| entry.host.clone()).collect();
        let ctx_keys = host_types.iter().map(ctx_key).collect();

        let (backends_ty, backends_def) = emit_backends(host_entries);

        let manifest = emit_manifest(config);
        let main_options = emit_main_options(config, manifest.is_some());

        Self {
            mode: config.mode,
            host_types,
            ctx_keys,
            backends_ty,
            backends_def,
            main_options,
            manifest,
            link_loader: config.link_loader,
        }
    }
}

/// Emit the `omnia::MainOptions` method chain passed to `omnia::main`; a
/// compiled-in manifest rides the generated `manifest()` accessor, and keys
/// the invocation omits contribute no calls.
fn emit_main_options(config: &Config, has_manifest: bool) -> TokenStream {
    let mode = config.mode.tokens();
    let manifest = has_manifest.then(|| quote! { .manifest(manifest()) });

    quote! {
        omnia::MainOptions::new(#mode)
            #manifest
    }
}

/// Emit the `omnia::ManifestSource` for the compiled-in deployment
/// manifest: `Path` for a `config:` expression, `Inline` for the inline
/// manifest keys, nothing when neither is declared.
fn emit_manifest(config: &Config) -> Option<TokenStream> {
    if let Some(expr) = &config.config_file {
        return Some(quote! {
            omnia::ManifestSource::Path(::std::path::PathBuf::from(#expr))
        });
    }
    if config.manifest.is_empty() {
        return None;
    }

    let builder = emit_manifest_builder(&config.manifest);
    Some(quote! {
        omnia::ManifestSource::Inline(#builder)
    })
}

/// Emit the fluent `omnia::Manifest` builder chain for the inline keys.
fn emit_manifest_builder(manifest: &ManifestSpec) -> TokenStream {
    let interfaces = manifest.interfaces.iter().map(|interface| {
        quote! {
            .link([#interface])
        }
    });

    let locations = manifest.locations.iter().map(|location| match location {
        LocationSpec::Path { name, path } => quote! {
            .locations([omnia::Location::path(#name, #path)])
        },
        LocationSpec::Registry(endpoint) => quote! {
            .locations([omnia::Location::registry(#endpoint)])
        },
    });

    let guests = manifest.guests.iter().map(|guest| {
        let id = &guest.id;
        let source = &guest.source;
        let http = &guest.routes.http;
        let messaging = &guest.routes.messaging;
        let websocket = &guest.routes.websocket;
        let command = guest.command.then(|| quote! { .command() });
        quote! {
            .guest(
                omnia::GuestEntry::new(#id, #source)
                    #(.route_http(#http))*
                    #(.route_messaging(#messaging))*
                    #(.route_websocket(#websocket))*
                    #command
            )
        }
    });

    let mounts = manifest.mounts.iter().map(|mount| {
        let name = &mount.name;
        let path = &mount.path;
        let writable =
            mount.writable.as_ref().map_or_else(|| quote!(false), ToTokens::to_token_stream);
        quote! {
            .mounts([omnia::Mount {
                name: ::std::string::String::from(#name),
                path: ::std::path::PathBuf::from(#path),
                writable: #writable,
            }])
        }
    });

    quote! {
        omnia::Manifest::new()
            #(#interfaces)*
            #(#locations)*
            #(#guests)*
            #(#mounts)*
    }
}

fn emit_backends(host_entries: &[HostEntry]) -> (TokenStream, TokenStream) {
    // Order-preserving dedup: `Vec::dedup_by` only removes *consecutive*
    // duplicates, so a backend shared by non-adjacent hosts would emit two
    // identically named struct fields. Parse validation guarantees rows
    // sharing a backend agree on connect options, so the first row's
    // options stand for the shared connection.
    let rows = host_entries.iter().map(|entry| (&entry.backend, entry.options.as_ref()));
    let mut seen = std::collections::HashSet::new();
    let backends: Vec<(&Path, Option<&Expr>)> =
        rows.filter(|(backend, _)| seen.insert(path_key(backend))).collect();

    let idents: Vec<Ident> = backends.iter().map(|(backend, _)| field_ident(backend)).collect();
    let types: Vec<&Path> = backends.iter().map(|(backend, _)| *backend).collect();

    if idents.is_empty() {
        return (quote! { () }, quote! {});
    }

    // `Host: Backend(options)` compiles the options in; a bare row connects
    // from the environment.
    let connects: Vec<TokenStream> = backends
        .iter()
        .map(|(ty, options)| {
            options.map_or_else(
                || quote! { <#ty as Backend>::connect() },
                |options| quote! { <#ty as Backend>::connect_with(#options) },
            )
        })
        .collect();

    let host_impls: Vec<TokenStream> = host_entries
        .iter()
        .map(|entry| host_impl(&entry.host, &field_ident(&entry.backend)))
        .collect();

    (
        quote! { Backends },
        quote! {
            use omnia::Backend;

            #[derive(Clone)]
            struct Backends {#(
                #idents: #types,
            )*}

            impl omnia::Backends for Backends {
                async fn connect() -> Result<Self> {
                    let (#(#idents,)*) = tokio::try_join!(
                        #(#connects,)*
                    )?;
                    Ok(Self { #(#idents,)* })
                }
            }

            #(#host_impls)*
        },
    )
}

fn path_key(path: &Path) -> String {
    path.to_token_stream().to_string()
}

/// One uniform bundle-accessor impl per `hosts:` row. The borrow shape rides
/// the carrier's `HostCtx::Borrow` — `&mut self.field` coerces to every
/// carrier's borrow (`&mut dyn Ctx`, `&dyn Ctx`, or `&mut dyn HttpBorrow`) —
/// so third-party hosts and re-exports work with no name surgery.
fn host_impl(host: &Path, field: &Ident) -> TokenStream {
    let ctx = ctx_key(host);
    quote! {
        impl omnia::Provides<#ctx> for Backends {
            fn borrow(&mut self) -> <#ctx as omnia::HostCtx>::Borrow<'_> {
                &mut self.#field
            }
        }
    }
}

/// The `Provides` key for a `hosts:` row: the host type itself, save one
/// special case. `wasi:http`'s linker-facing view trait (`WasiHttpView`) is
/// foreign — owned by `wasmtime-wasi-http` — so its `StoreCtx` blanket lives
/// in omnia core against the core-owned `HttpCtx` carrier, and the http row's
/// accessor must be keyed to that carrier. (Keying by an associated type on
/// the host — `<#host as HostBinding>::Ctx` — is not an option: coherence
/// does not normalize projections in impl headers across crates, so two such
/// impls are rejected as overlapping.) An aliased `WasiHttp` row that dodges
/// this match fails loudly at compile time: linking requires `WasiHttpView`,
/// whose blanket bound `Provides<HttpCtx>` is then unsatisfied.
fn ctx_key(host: &Path) -> TokenStream {
    let is_http = host.segments.last().is_some_and(|segment| segment.ident == "WasiHttp");
    if is_http { quote!(omnia::HttpCtx) } else { quote!(#host) }
}

fn field_ident(path: &Path) -> Ident {
    let Some(segment) = path.segments.last() else {
        return format_ident!("field");
    };

    let mut snake = String::new();
    for ch in segment.ident.to_string().chars() {
        if ch.is_uppercase() {
            if !snake.is_empty() {
                snake.push('_');
            }
            snake.extend(ch.to_lowercase());
        } else {
            snake.push(ch);
        }
    }

    format_ident!("{snake}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> Path {
        syn::parse_str(name).expect("valid path")
    }

    #[test]
    fn derives_field_ident() {
        assert_eq!(field_ident(&path("HttpDefault")).to_string(), "http_default");
        assert_eq!(field_ident(&path("KeyValueDefault")).to_string(), "key_value_default");
    }

    #[test]
    fn empty_host_entries() {
        let (ty, def) = emit_backends(&[]);
        assert_eq!(ty.to_string(), "()");
        assert!(def.is_empty());
    }
}
