## Unreleased

### Added

- Guest-requested plugin loading: the `omnia:plugins/loader` host capability.
  A guest whose world imports it can ask the host to load a component at run
  time — `load(package, location, digest?)` returns a plain `plugin` record
  (`id`, `digest`), a value carrying no lifecycle authority over the loaded
  component; component bytes never cross the interface in either
  direction. The host pipeline is trust-ordered: idempotency on
  (package, digest) → acquisition through the deployment's compiled-in
  acquirer → sha256 pin verification (before any wasmtime validation;
  unpinned loads report the resolved digest for trust-on-first-use) → typed
  refusal of native/pre-compiled bytes → safe `GuestArtifact::wasm`
  validation only → refusal unless the component exports a declared plugin
  interface → registration under the package identity. Refusals are typed
  by the caller's remedy (`refused` for a wrong request or deployment,
  `unavailable` for a retryable acquisition failure, `already-active` for an
  identity conflict, `internal` for a host fault), with the description
  naming the specific cause; a deployment guest's identity can never be
  re-bound, and a conflicting re-pin of an active package refuses.
  Acquisition policy is a pair of
  composition-root slots, one per location kind (`RegistrySource`,
  `PathSource`), installed through `Plugins::install` from the
  `Wiring::extend` hook; the `runtime!` macro's declarative
  `plugins: { locations: [...] }` list (or a manifest's `[[location]]`
  entries) carries them as deployment data (`Location`) that the
  generated hook installs through `Plugins::install_declared`. A load names
  its `Origin` — a registry endpoint or a location-relative path — and
  routes structurally by kind; an empty slot refuses typed. The built-in
  acquirers ship in `omnia-plugin` and re-export: `PathMounts` (named
  directory roots opened fail-fast at startup, read fresh on every load)
  and `RegistryClient` (exact `namespace:name@version` references from a
  declared default registry endpoint, verified against the registry's
  content digest, fresh-release-preferred when `RegistryClient::cached`
  attaches a store — `ContentStore` for digest-keyed bytes, `ReleaseStore`
  for per-registry release records). The loader
  links once on the shared linker when a deployment declares `plugins`;
  wasmtime wires it only
  into guests whose world imports `omnia:plugins/loader`. See
  [docs/security-model.md](docs/security-model.md#guest-requested-plugin-loading-omniapluginsloader).
- The requester surface for the loader, in `omnia-guest`'s new `plugins`
  module: a `Plugins` capability trait (WASI-backed default body on `wasm32`
  over the crate's own `omnia:plugins/loader` bindings, bare natively so
  suites script loads), the `WasiPlugins` zero-sized provider, shared
  `PluginRef`/`Digest`/`Location` types compiled on both targets (`Digest`
  validates and canonicalizes `sha256:<hex>` pins, with serde support), a
  `Plugin` handle carrying the routed identity and resolved digest, typed
  refusals mirroring the WIT error variant with kebab-case `code()`
  discriminants and a conversion into the guest `Error` taxonomy
  (`unavailable` → `BadGateway`, `internal` → `ServerError`, every other
  refusal → `BadRequest`), and `PluginCache` — ensure-once memoization of
  handles by package identity (never bytes; a conflicting re-pin refuses
  `already-active`, mirroring the host), itself a `Plugins` provider so a
  caller bounded on the capability memoizes without naming the cache. No
  consumer vocabulary anywhere: any requester-class world can use it.

- `hosts:` rows accept compiled-in connect options: `Host: Backend(options)`
  lowers to `Backend::connect_with(options)` instead of the env-sourced
  `Backend::connect()`. Use it to compile configuration into the binary — a
  fixed storage root (e.g. a project CAS path), or a scripted test backend
  carrying state without statics. Rows sharing a backend type share one
  connection, so their options must be written identically on every row (or
  omitted on every row) — a mismatch is a spanned compile error, as are
  empty parentheses.

  ```rust
  hosts: {
      WasiKeyValue: Filesystem(FilesystemOptions::at(".omnia/storage")),
      WasiBlobstore: Filesystem(FilesystemOptions::at(".omnia/storage")),
  }
  ```

- `omnia-test`, published for the first time: test doubles, a component
  runtime harness, and a `wasm32-wasip2` fixture pipeline for code built on
  omnia, behind three additive features with no default — a consumer names
  the rung it uses, and a build-dependency line that forgets
  `default-features = false` still pulls nothing but `std`.
  - `guest`: a native double per `omnia_guest` capability trait —
    `Scripted: Model`, `ScriptedLoader: Plugins` (per-package digests and
    refusals, plus `defaulting(digest)` for every unscripted, unpinned load
    in place of the per-package placeholder), `Memory: StateStore +
    BlobStore` (with `Namespaced`; the `BlobStoreExt` operations derive from
    the primitives), `MemoryDocs` over the docstore default,
    `ScriptedTables`, `MatchedHttp`, `Sink: Publish + Broadcast`,
    `MapConfig`, `FixedIdentity` — plus `omnia_test::provider!`, the native
    twin of `omnia_guest::provider!` (same name, same grammar; a test
    declaration differs from `src/lib.rs` by the crate path alone, seeded
    with the default double per capability, `StateStore` and `BlobStore`
    sharing one `storage` field) and `delegate!` (delegating capability impls
    to fields, with a bracketed generic header).
  - `host`: `Deployment`, an overlay on a manifest — built from nothing with
    `Deployment::new()` or from a `runtime!` module's compiled-in
    `manifest()` — that rewrites the guest set, mounts, plugin interfaces,
    the command guest and the `.` path location, and runs through the host
    list you name (`run_host`, `run`) or the module's generated `Hooks`
    (`run_with`). `Backends` bundles the twelve in-memory defaults with the
    model swappable for any `WasiModelCtx` via the generic
    `Backends::model`; `defaults()` is deterministic — no environment
    variable read, no socket opened: config answers from the map seeded by
    `Backends::config([..])` alone, the websocket backend serves no
    listener, and the HTTP client and `SQLite` connection (one private
    `:memory:` database per bundle) are built from fixed options.
    `ScriptedModel` scripts completions whose steps (tool calls and
    workspace `read`/`write`/`list`) run ahead of the answer, recording
    every exchange and the `local_path` lent per turn; an overrun answers
    the guest softly and fails the test at `assert_exhausted()`. `Scratch`
    gives per-test directories and mounts.
  - `build`, `std`-only: `Components` runs the nested cargo build a
    consumer's `build.rs` needs and writes `gen.rs` with one path constant
    per program and a `foreach_<group>!` completeness macro per group.
    Programs are `[[example]]` targets (`examples`, `scan`) or the shipped
    `cdylib` packages themselves (`packages`, `scan_packages` over a
    directory of crates — constants read `<GROUP>_<NAME>`, arms are the
    crate name), and `extra_package` builds a driver guest beside them with
    a constant but no arm; `Program` gains a `constant` field.
  Both scripted models share one `Script` core, so the same script reads
  identically at the handler and component rungs. `cargo make ci` now checks
  each feature alone and in combination (`features`, `hack`) and guards the
  `build` dependency tree (`tree-guard`).

- `omnia_guest::command!(entry)`: wires an `async fn` returning `()` or
  `Result<(), u8>` (via the `IntoExit` trait) as the guest's `wasi:cli/run`
  export, run through `command::execute_wasi` so telemetry is initialized
  and flushed around it. The e2e scenario programs use it in place of the
  deleted `test_programs::run!`; because the export now imports
  `omnia:otel`, `omnia_test::host::Deployment::run_host` links `WasiOtel`
  beside the host under test and requires `B: Provides<WasiOtel>`.

- `omnia_wasi_websocket::WebSocketDefault::new()` (and `Default`) is public:
  the backend without a listener, whose `connect()` still yields a client.
  `omnia_wasi_http::ConnectOptions` and `omnia_wasi_sql::ConnectOptions` are
  re-exported so `connect_with` can be called with fixed options instead of
  `connect()` reading `HTTP_CONNECT_TIMEOUT` / `SQL_DATABASE`.

- Transport-neutral failure and encoding surface in `omnia_guest::api`:
  `ErrorBody { error, message }` is the one wire body for a failed
  invocation (`From<&Error>`: `code()` → `error`, `description()` →
  `message`); `Error::exit_code()` sits beside `Error::status()` with the
  1:1 exit map (`BadRequest` 1, `NotFound` 2, `ServerError` 3,
  `BadGateway` 4); `Format { Text, Json }` with
  `Format::encode(&body, render)` produces an `Encoded { bytes, media_type }`
  — text through a `Fn(&T, &mut dyn fmt::Write) -> fmt::Result` render
  closure as `text/plain; charset=utf-8`, or pretty JSON with a trailing
  newline as `application/json` — and `Encoded` implements axum's
  `IntoResponse` (200 with the media type as `Content-Type`).

### Changed

- Guest handlers are fns bound at the route, over an owned context.
  `omnia_guest::api::Context<P>` drops its lifetime and `Copy`: it owns
  `Arc<str>` / `Arc<P>` clones plus the `Metadata`, exposes `owner()` and
  `provider()` accessors over private fields (the public `metadata` field
  stays), and gains `Context::new(owner, provider, metadata)` so a handler
  can be unit-tested without a `Client`. `Handler<P>` implemented on the
  input type becomes `Handler<P, I>` with one blanket impl over every
  `async fn(I, Context<P>) -> Result<O, E>` (and every `Clone` closure of
  that shape, so a closure can carry configuration into a route); the
  trait method is `call(self, input, context)`, and implementing it by hand
  on a local non-fn type is the documented escape hatch. Route
  constructors take the handler value instead of a type parameter:
  `get(handler)` / `delete(handler)` / `post(handler)` / `put(handler)` /
  `patch(handler)`, `handle_with(filter, handler, decode, encode)` with the
  decoder returning `Result<I, DecodeError>` and the encoder
  `Fn(F::Output) -> Response`, `consume(handler)`, and
  `consume_with(handler, decode)`;
  `Client::call(handler, input, &metadata)` takes the handler first.
  Nothing on the wire changes (JSON envelope, `HttpError`, `DeliveryError`,
  exit codes). The `#[omnia_guest::handler]` proc-macro and its
  `omnia_guest::handler` re-export are deleted — rustc's own diagnostics
  name a mis-shaped fn — and `omnia-guest` no longer depends on
  `omnia-guest-macros`, which now provides only `#[instrument]` (still
  reaching guests as `omnia_wasi_otel::instrument`).

  ```rust
  // before                                        // after
  impl Handler<P> for CreateItem {                  async fn create_item(
      type Output = ItemReply;                          input: CreateItem, context: Context<P>,
      type Error = Error;                           ) -> Result<ItemReply, Error> {
      async fn handle(                                  let cfg = context.provider().config();
          self, context: Context<'_, P>,                // ...
      ) -> Result<ItemReply, Error> {               }
          let cfg = context.provider.config();
          // ...
      }
  }
  .route("/items", post::<CreateItem, P>())         .route("/items", post(create_item))
  .route("t", consume::<CreateItem>())              .route("t", consume(create_item))
  client.call(input, &metadata)                     client.call(create_item, input, &metadata)
  ```
- `omnia` is the composition root (deployment assembly, process
  lifecycle, optional-crate composition); `omnia-core` is the
  live-runtime SDK a capability crate targets; `omnia-cli` is a leaf
  grammar crate with no `omnia-*` dependencies. `DeploymentBuilder` is
  no longer typestate-parameterized: `build` is the safe wasm path and
  `unsafe build_trusted` replaces the `Precompiled` typestate's unsafe
  `build` (`WasmOnly` / `Precompiled` and `precompiled()` are deleted).
  `omnia::run` / `run_with` take a built `Deployment` (the
  `runtime!`-generated wrappers still take a `DeploymentBuilder` and
  build first); `run_precompiled` is deleted. `Runtime::new` /
  `with_backends` and their link/extend closures are deleted:
  `Deployment::assemble` builds a `Runtime` from `RuntimeParts` via
  `Runtime::from_parts`. Artifact loading is one `GuestArtifact::load`;
  `Source` decides trust by content (native bytes never reach wasmtime
  on the safe path), and `Runtime::admit` no longer sniffs ELF itself.
  `Manifest`, `DeploymentBuilder`, `Deployment`, `Wiring`, `Backends`,
  `Mode`, `run` / `run_with`, `main`, and `compile` live in `omnia`;
  `omnia-core` keeps the live handle (`Runtime`, `Registry`, `admit`,
  `Extensions`, dispatch, telemetry).
- The `run` command-line grammar moves out of `omnia-core` into a new
  `omnia-cli` crate, which the `omnia` facade's `cli` feature now selects
  (`cli = ["dep:omnia-cli"]`). `omnia-core` no longer has a `cli` feature or
  a `clap` dependency; its entry point serves direct commands only.
  `omnia_core::{Cli, Command, Parser}` are now `omnia_cli::{Cli, Command,
  Parser}` — the facade paths `omnia::Cli`, `omnia::Command`, and
  `omnia::Parser` are unchanged, as is everything the `runtime!` macro emits.
  A direct-command binary built with `--no-default-features` now links no
  `clap`.
- The `omnia:plugins/loader` capability (`omnia-plugin`) sits behind a new
  non-default `plugin` feature on the `omnia` facade
  (`plugin = ["dep:omnia-plugin"]`), mirroring `cli`. The facade paths
  (`omnia::WasiPlugins`, `omnia::Plugins`, `omnia::PathMounts`,
  `omnia::LoadError`, …) and the `runtime!` syntax are unchanged; a
  deployment that declares `plugins: { locations: [...] }`, or a bare
  `plugins: {}` beside `config:` for the TOML's `[[location]]` entries,
  must enable it (`omnia = { version = "...", features = ["plugin"] }`) or
  the expansion fails to compile naming `omnia::WasiPlugins`. A default
  build carries no loader and no HTTP/OCI registry stack (`wasm-pkg-client`
  and its ~120 transitive crates are gone), and refuses a manifest with
  `[[location]]` entries at startup. The macro links the loader host and
  emits `Wiring::extend` only for those two shapes: an interfaces-only
  `plugins: { interfaces: [...] }` block no longer links a loader that
  refused every load, so a guest importing `omnia:plugins/loader` in such a
  deployment now fails at instantiation rather than per load; a bare
  `plugins: {}` without `config:` is now a compile error instead of a
  silent no-op. `omnia::LoadError` is owned by `omnia-plugin` rather than
  being the `bindgen!`-generated WIT error: same variants, `Display`, and
  `Clone`/`Debug`/`PartialEq`/`Eq`/`Error`, minus wasmtime's
  `Lift`/`Lower`/`ComponentType` impls.
- Every `omnia-guest` capability trait is implemented for `Arc<T>`, `&T`,
  and `Box<T>` where `T` implements it, forwarding on both targets, so a
  provider field may hold a double behind a shared handle and a
  `P: StateStore` bound accepts `Arc<Memory>` with no `delegate!`.
- `BlobStore` splits: the ten primitives (`get`, `put`, `delete`, `list`,
  `get_range`, `object_info`, `create_container`, `delete_container`,
  `container_exists`, `container_info`) stay on `BlobStore`; `has`,
  `delete_objects`, `clear`, `copy_object`, and `move_object` move to
  `BlobStoreExt`, a blanket-implemented extension trait whose native bodies
  compose the primitives and whose `wasm32` bodies remain one host call
  each. Migration: delete those five methods from any hand-written
  `BlobStore` impl (they are no longer trait members) and import
  `omnia_guest::BlobStoreExt` where a call site uses them. `Memory` and
  `Namespaced` in `omnia-test` drop their overrides accordingly.
- The `omnia` facade is the whole embedder dependency, in documentation as
  well as in code. Its re-exports are `#[doc(inline)]`, so rustdoc renders
  every runtime and plugin item as `omnia::…` instead of a wall of
  `pub use omnia_core::…` lines whose pages — and every path a reader copies
  from them — spelled the underlying crate; `WasiPluginsCtxView` joins the
  plugin re-exports so that surface is complete. The generated runtime
  module imports `omnia::anyhow::Result` rather than `anyhow::Result`, so
  `runtime!` no longer requires `anyhow` in the embedder's own `Cargo.toml`
  (`omnia::anyhow` is now a documented re-export, being the error vocabulary
  of `Backend` and `Wiring`, as is `omnia::futures`, whose `BoxFuture` the
  `ContentStore`/`ReleaseStore`/`PathSource`/`RegistrySource` seams return).
  `omnia-plugin`'s docs no longer tell store
  implementors to depend on it directly — `ContentStore`/`ReleaseStore` and
  the `Backend`/`NoOptions` a `cache:` store also needs all reach embedders
  through `omnia`; depending on `omnia-core` or `omnia-plugin` is only for
  building another capability crate.
- Plugin loads are lock-free and race-safe. The loader's (package, digest)
  idempotency record now rides the registry entry itself (`Guest::digest`,
  recorded by `Runtime::admit` from the admitted bytes), so the attestation
  can never outlive or misdescribe the guest it names — an identity
  deregistered and re-registered by the embedder no longer answers a pinned
  re-load with a stale digest. The loader's shadow digest map and the global
  load mutex are deleted; concurrent loads race through `admit`, whose
  atomic publication reports the loser via the new
  `AdmitError::AlreadyRegistered` variant, resolved against the winner's
  recorded digest (idempotent success on a match, `already-active`
  otherwise). `sha256_digest` moves to `omnia-core` (still re-exported from
  `omnia`).
- Acquirers refuse honestly: `RegistrySource` and `PathSource` return the
  loader's typed `LoadError` — `refused` for an authoritative "no" (a
  malformed reference, a package or path the source does not serve),
  `unavailable` for a failure a retry may clear — so an unknown package no
  longer reports as retryable.
- The runtime core drops "plugin" from its vocabulary: the manifest accessor
  `Manifest::plugin_interfaces()` is renamed to `link_interfaces()`, and
  admission refusals name "link interfaces". The config surface is
  unchanged — the TOML `plugins = [...]` list, the macro `plugins:` block,
  and the CLI `--plugins` flag keep their names; only `omnia-plugin` speaks
  "plugin".
- The `omnia` crate splits into `omnia-core` (the runtime spine: deployment,
  registry, dispatch, stores, telemetry, CLI) and a thin `omnia` facade that
  re-exports core, `omnia-plugin`, and the `runtime!` macro under the
  existing paths — embedder imports are unchanged. `omnia-plugin` is now the
  whole plugins capability: the loader WIT and `WasiPlugins` host binding,
  the `Plugins` load path, digest policy, and the acquisition seam all
  live there, built on two intentional core seams that future capability
  crates reuse: `Runtime::admit` (the one privileged operation — safe
  validation, seam-export check, atomic registration; typed `AdmitError`)
  and the `Extensions` typemap (capability state installed by the new
  `Wiring::extend` hook — replacing `Wiring::acquirer` — and read back from
  stores through `HasExtensions`; state that calls back into the runtime
  holds a `WeakRuntime` via `Runtime::downgrade`). `Runtime::new`'s third
  parameter is now the extend hook (`FnOnce(&Runtime<B>) -> Result<()>`);
  `StoreConfig::loader`/`StoreBase::loader` are replaced by the `extensions`
  handle, and the loader's digest record rides the registry entry itself
  (`Guest::digest`, recorded at admission), living and dying with the guest
  it attests.
- Host wiring is trait-carried, not name-derived (pre-1.0 hard cut, no
  aliases). Omnia core gains `HostCtx` (the host's borrow shape and view
  assembly, `Borrow<'a>` GAT + `view`), `Provides<H>` (the one
  bundle-accessor trait), and `StoreView<H>` (the one store-side view trait,
  blanketed on `StoreCtx<B>` for every `B: Provides<H>`). The per-host
  `Wasi*View` traits, `Has*` accessor traits, and per-host `StoreCtx`
  blankets are deleted; `wasi_view!` now emits the `CtxView` struct plus the
  host's `HostCtx` impl, and host `add_to_linker` accessors ride
  `T: StoreView<WasiX>` / `T::view`. The `runtime!` macro emits one uniform
  `impl omnia::Provides<...> for Backends` per `hosts:` row — no more name
  surgery from the host ident, so re-exports and third-party hosts wire the
  same way as first-party ones, and `wasi:config`'s shared-borrow shape is
  carried by its own `HostCtx` impl instead of a codegen special case. One
  special case survives, documented in codegen: `wasi:http`'s view trait is
  foreign (`wasmtime-wasi-http`), so its row is keyed to the core-owned
  `omnia::HttpCtx` carrier (`HasHttp` is replaced by the backend-level
  `HttpBorrow` trait), and a dodged match now fails at compile time rather
  than silently. Hand-built bundles implement `Provides<WasiX>` directly.

  ```rust
  // before                              // after
  impl HasModel for Backends {           impl omnia::Provides<WasiModel> for Backends {
      fn model_ctx(&mut self)                fn borrow(&mut self)
          -> &mut dyn WasiModelCtx {             -> &mut dyn WasiModelCtx {
          &mut self.0                            &mut self.0
      }                                      }
  }                                      }
  ```
- The `runtime!` macro serves every `hosts:` row uniformly through
  `Server::run`: capability hosts resolve immediately via the no-op default,
  trigger servers loop until shutdown. The macro's string-matched server
  list and the unread `Server::IS_SERVER` const are deleted; a third-party
  trigger host's `run` is now actually served instead of silently skipped.
- The `runtime!` macro's `plugins:` key grows from a bracketed list to a
  block: `plugins: { interfaces: [...], locations: [...] }`. The
  declarative `locations:` list is the acquisition policy — named path roots
  and at most one registry endpoint — carried as manifest data
  (`Manifest::locations`, `Location`; `[[location]]` in `omnia.toml`)
  and installed by the generated `Wiring::extend` through
  `Plugins::install_declared`, which folds path entries into `PathMounts`
  and the registry entry into a cacheless `RegistryClient`, slotted by
  location kind. Both keys are optional (a deployment that only does
  host-mediated dispatch needs no acquirer; loads then refuse typed at run
  time); an empty `locations:` list and a second `registry` entry are
  spanned compile errors, and the bare-list form is a compile error naming
  the block shape. Declaring the block also links the `omnia::WasiPlugins`
  loader host. Because locations are manifest data they conflict with
  `config:` like every other inline key; a config-file deployment declares
  `[[location]]` entries in the TOML. The bundle-carried `cache:` option is
  gone (a spanned diagnostic names the migration): a store-backed registry
  acquirer is installed by hand — `Plugins::install` over
  `RegistryClient::new(..).cached(store)` from a custom `Wiring::extend`.

  ```rust
  // before                              // after
  plugins: ["omnia:link/echo"],          plugins: {
                                             interfaces: ["omnia:link/echo"],
                                             locations: [        // optional
                                                 { name: ".", path: "." },
                                                 { registry: "ghcr.io" },
                                             ],
                                         },
  ```
- The `runtime!` macro's wiring is reachable from tests. The generated
  `Hooks` is now `pub` and generic — `impl<B> Wiring<B> for Hooks where B:
  Provides<...>` for each declared host — and two items join `main` and
  `run` in the re-export: `manifest()` returns the compiled-in
  `ManifestSource`, and `async fn run_with<B>(builder, backends)` drives a
  deployment through `Hooks` over a bundle already in hand, connecting
  nothing. Behind them, `Wiring<B>` is bounded on `B: Clone + Send + Sync +
  'static` instead of `Backends` (a `Backends` bundle still satisfies it),
  `omnia::run_with` and `Runtime::with_backends` are the connect-free twins
  of `run` and `Runtime::new`, and `Runtime::plugin_locations` exposes the
  deployment's declared locations. `omnia_test::host::Deployment` builds on
  this as an overlay: `Deployment::from(runtime::manifest())` takes the
  binary's manifest as its base, `path_root` rewrites the `.` location, and
  `run_with::<runtime::Hooks, _>(backends)` runs the production `link`,
  `extend`, and `serve` over `omnia_test::host::Backends`.

  ```rust
  mod runtime { omnia::runtime!({ plugins: { locations: [{ name: ".", path: "adapters" }] }, guests: [...], hosts: {...} }); }

  let status = Deployment::from(runtime::manifest())
      .path_root(scratch.path())
      .run_with::<runtime::Hooks, _>(Backends::defaults().await)
      .await?;
  ```
- The host-mediated interface list is renamed from `dispatch` to `plugins`:
  a top-level `plugins = [...]` in TOML, a top-level `plugins: [...]` key in
  the `runtime!` macro, the fluent `Manifest::plugins(...)` setter (with the
  `Manifest.plugins` field and the `Manifest::link_interfaces()` accessor),
  and the CLI flag `--plugins` (replacing `--dispatch`, no alias). Stale keys
  fail loudly: a leftover top-level `dispatch` (or `link`) is a parse/compile
  error, and `plugins` misplaced on a guest entry is rejected with a pointed
  diagnostic. Behavior is unchanged: listed interfaces are polyfilled onto
  the shared linker at assemble (an exporter may arrive later via
  `Runtime::register`), and the selector still picks the target guest by
  routing id at call time. The dispatch *mechanism* keeps its names —
  `crates/omnia/src/dispatch`, `serve_links`, `DispatchHandle`,
  `MAX_DISPATCH_DEPTH`, and the `omnia:link` WIT packages are untouched.

  ```toml
  # before                            # after
  dispatch = ["omnia:link/echo"]      plugins = ["omnia:link/echo"]
  ```
- The host-mediated interface list is renamed from `link` to `dispatch`
  and is deployment-wide only: a top-level `dispatch = [...]` in TOML, a
  top-level `dispatch: [...]` key in the `runtime!` macro, the fluent
  `Manifest::dispatch(...)` setter, and the CLI flag `--dispatch` (replacing
  `--link`, no alias). The per-guest form (`GuestEntry.link` in TOML,
  `link:` on a macro guest entry, `GuestEntry::link()`) is removed — the
  linker is shared, so per-guest lists always flattened into one
  deployment-level grant and never enforced a per-guest ACL. Stale keys
  fail loudly: a leftover `link` (top-level or per-guest) or a `dispatch`
  misplaced on a guest entry is a parse/compile error. Behavior is
  unchanged: listed interfaces are polyfilled onto the shared linker at
  assemble (an exporter may arrive later via `Runtime::register`), and the
  selector still picks the target guest by routing id at call time. WIT
  packages (`omnia:link`) and internals (`serve_links`,
  `DispatchHandle::links`) keep their names.

  ```toml
  # before                            # after
  [[guest]]                           dispatch = ["omnia:link/echo"]
  id = "router"
  source.path = "./router.wasm"       [[guest]]
  link = ["omnia:link/echo"]          id = "router"
                                      source.path = "./router.wasm"
  ```
- Routes are now guest-owned: each `[[guest]]` entry declares the routes
  targeting it (`routes.http` / `routes.messaging` / `routes.websocket`
  pattern lists in TOML, a `routes: { http: [...], ... }` block per guest
  entry in the `runtime!` macro, `GuestEntry::route_http` /
  `route_messaging` / `route_websocket` programmatically), with the
  declaring guest as the implicit target. The top-level `[[route.*]]`
  tables, the macro's top-level `routes:` key, the `Manifest::route_*`
  setters, and the `RouteSpec` / `HttpRoute` / `TopicRoute` types are
  removed; a stale top-level route section now fails manifest parsing.
  Routing behavior is unchanged: per-guest lists aggregate into the same
  per-trigger tables (longest-prefix HTTP, first-match NATS-style
  patterns, capability catch-all when a trigger has no routes).

  ```toml
  # before                            # after
  [[guest]]                           [[guest]]
  id = "api"                          id = "api"
  source.path = "./api.wasm"          source.path = "./api.wasm"
                                      routes.http = ["/api"]
  [[route.http]]
  prefix = "/api"
  guest = "api"
  ```
- Removed the `runtime!` macro's `program:` key: `mode: command` with a
  compiled-in deployment (`config:` or inline manifest keys) is now a
  direct command by default — raw argv passthrough with the reserved
  `--debug` / `--quiet` host log flags, no host `run` grammar. The program
  name (telemetry and guest `argv[0]`) defaults to the manifest name (first
  `[[guest]]` id). Command-mode binaries without a compiled-in deployment
  keep the `run` grammar
- Removed the `command_guest:` key and its plumbing
  (`DeploymentBuilder::command_guest`, `Runtime::with_command_guest`,
  `MainOptions::command_guest`): command mode routes to the sole static
  `wasi:cli/run` exporter, or to the guest entry marked `command = true`
  (macro `command: true`, `GuestEntry::command()`); at most one guest may
  carry the mark. The resolver-supplied command guest (fully dynamic
  deployment, empty guest set) is gone with the key — a direct command
  always compiles its manifest in
- Removed the late-binding deployment plumbing: the resolve-on-miss pull
  layer (`GuestResolver`, `Runtime::ensure_guest`, the single-flight
  machinery, macro `resolver:`, `DeploymentBuilder::resolver`,
  `Runtime::with_resolver`), the `http_paths` trigger hook (macro
  `http_paths:`, `DeploymentBuilder::http_paths`,
  `Runtime::with_http_paths`, `RoutingPolicy` / table-only routing,
  `Runtime::route_http`, `RouteRefusal`), and pre-bound HTTP listener
  adoption (macro `http_listener:`, `DeploymentBuilder::http_listener`,
  `Runtime::take_http_listener`). A registry miss is a dispatch error and
  an unrouted HTTP path is a 404; the HTTP trigger always binds
  `HTTP_ADDR`. Push registration stays: `Runtime::register` / `deregister`
  and `DeploymentBuilder::dynamic()` are the way a registry grows after
  boot, with registered guests reachable via host-mediated link dispatch
  and `Dispatcher::invoke`
- `HttpError::from(omnia_guest::Error)` now emits the JSON `ErrorBody`
  (`{"error":"<code>","message":"<description>"}`, `application/json`) at
  the variant's status instead of a plain-text `code: …, description: …`
  body, so HTTP clients read the same `error` discriminant every transport
  emits. This also covers `From<DecodeError>` (400 with
  `error == "invalid_request"`) and the `anyhow::Error` conversion when its
  chain contains an `omnia_guest::Error`; a foreign `anyhow` error still
  produces a plain-text 500.

## 0.35.0

Released 2026-07-25

Paired production-backends release: [omnia-backends 0.29.x](https://github.com/augentic/omnia-backends/blob/main/RELEASES.md).

### Added

- Multi-guest registry: one process hosts many Wasm components on a shared
  engine/linker, selected by opaque `GuestId`, with instance-per-call
  instantiation, route tables, and host-mediated guest-to-guest linking
- Resolve-on-miss `GuestResolver` so missing guests can be faulted in at
  dispatch time (HTTP fallback, command routing, single-flight per identity)
- `runtime!` keys for resolver-backed deployments: `resolver:`,
  `command_guest:`, `program:`, and compile-time `config:` fallback
- Embedded guest bytes (`include_bytes!` / `Source::embedded`) for
  single-binary hosts alongside path-sourced guests
- Dynamic `Runtime::register` / `deregister` after bootstrap, including late
  import polyfill for guests admitted after static assembly
- Pooling allocator enabled by default, with environment-driven
  `RuntimeOptions` shared by `omnia compile` and `omnia create`
- `omnia-wasi-model` host interface and workspace packaging for model /
  working-tree workflows
- Stateless MCP stack in `omnia-guest` (JSON-RPC + Streamable HTTP) and
  testkit helpers for MCP grant recording
- Concurrent (`async func`) host-mediated link dispatch via
  `func_new_concurrent` / `send_concurrent`
- `omnia-testkit` (dev-only) and an integration-first testing posture

### Changed

- `omnia-sdk` renamed to `omnia-guest`; host/guest macro crates split into
  `omnia-host-macros` and `omnia-guest-macros`; `omnia-otel` folded into
  `omnia`; `omnia-wasi-jsondb` replaced by `omnia-wasi-docstore`
- Deployments center on `Manifest` / `omnia.toml` (`OMNIA_CONFIG`), with
  `[[mount]]` working-tree preopens and typed `DeploymentBuilder` paths
  (safe wasm vs trusted precompiled)
- Upgraded `wasmtime` to 47.0.2 (and matching `wasmtime-wasi*`),
  `wit-bindgen` to 0.60, `wasip3` to 0.7, and `cap-std` / `cap-fs-ext` to 4.x
- Compile-affecting runtime toggles now include `DEBUG_SYMBOLS` and
  `GENERATE_ADDRESS_MAP` so AOT compile and load stay aligned

<!-- Release notes generated using configuration in .github/release.yaml at main -->

## What's Changed
* Bump to 0.35.0 by @augentic-releases[bot] in https://github.com/augentic/omnia/pull/202
* Instrumentation fix by @andrewweston in https://github.com/augentic/omnia/pull/203
* Instance pooling by @andrewweston in https://github.com/augentic/omnia/pull/204
* Guest registry by @andrewweston in https://github.com/augentic/omnia/pull/205
* Implement wasi model by @andrewweston in https://github.com/augentic/omnia/pull/206
* Specify readiness testing by @andrewweston in https://github.com/augentic/omnia/pull/207
* MCP server for cursor-agent by @andrewweston in https://github.com/augentic/omnia/pull/208
* Post-upgrade testing and code review by @andrewweston in https://github.com/augentic/omnia/pull/209
* Async guest-2-guest linking by @andrewweston in https://github.com/augentic/omnia/pull/210
* style fenced code by @andrewweston in https://github.com/augentic/omnia/pull/211
* Specify-driven refactoring by @andrewweston in https://github.com/augentic/omnia/pull/212
* Streamline testing by @andrewweston in https://github.com/augentic/omnia/pull/213
* Replay by @andrewweston in https://github.com/augentic/omnia/pull/214
* MCP grants by @andrewweston in https://github.com/augentic/omnia/pull/215
* Runtime flexibility by @andrewweston in https://github.com/augentic/omnia/pull/216
* improve runtime config by @andrewweston in https://github.com/augentic/omnia/pull/217
* Dynamic guest resolver by @andrewweston in https://github.com/augentic/omnia/pull/218
* Dynamic guest resolver in runtime! by @andrewweston in https://github.com/augentic/omnia/pull/219
* Embed guest bytes by @andrewweston in https://github.com/augentic/omnia/pull/220
* Update to wasmtime 47.0.2 by @andrewweston in https://github.com/augentic/omnia/pull/221

**Full Changelog**: https://github.com/augentic/omnia/compare/v0.34.0...v0.35.0

---

Release notes for previous releases can be found on the respective release branches of the repository.

<!-- ARCHIVE_START -->
* [0.35.x](https://github.com/augentic/omnia/blob/release-0.35.0/RELEASES.md)
* [0.34.x](https://github.com/augentic/omnia/blob/release-0.34.0/RELEASES.md)
* [0.33.x](https://github.com/augentic/omnia/blob/release-0.33.0/RELEASES.md)
* [0.32.x](https://github.com/augentic/omnia/blob/release-0.32.0/RELEASES.md)

- [0.31.x](https://github.com/augentic/omnia/blob/release-0.31.0/RELEASES.md)
- [0.30.x](https://github.com/augentic/omnia/blob/release-0.30.0/RELEASES.md)
- [0.29.x](https://github.com/augentic/omnia/blob/release-0.29.0/RELEASES.md)
- [0.28.x](https://github.com/augentic/omnia/blob/release-0.28.0/RELEASES.md)
- [0.27.x](https://github.com/augentic/omnia/blob/release-0.27.0/RELEASES.md)
- [0.25.x](https://github.com/augentic/omnia/blob/release-0.25.0/RELEASES.md)
- [0.23.x](https://github.com/augentic/omnia/blob/release-0.23.0/RELEASES.md)
- [0.22.x](https://github.com/augentic/omnia/blob/release-0.22.0/RELEASES.md)
- [0.21.x](https://github.com/augentic/omnia/blob/release-0.21.0/RELEASES.md)
- [0.20.x](https://github.com/augentic/omnia/blob/release-0.20.0/RELEASES.md)
- [0.20.x](https://github.com/augentic/omnia/blob/release-0.20.0/RELEASES.md)
- [0.19.x](https://github.com/augentic/omnia/blob/release-0.19.0/RELEASES.md)
- [0.18.x](https://github.com/augentic/omnia/blob/release-0.18.0/RELEASES.md)
- [0.17.x](https://github.com/augentic/omnia/blob/release-0.17.0/RELEASES.md)
- [0.16.x](https://github.com/augentic/omnia/blob/release-0.16.0/RELEASES.md)
- [0.15.x](https://github.com/augentic/omnia/blob/release-0.15.0/RELEASES.md)
