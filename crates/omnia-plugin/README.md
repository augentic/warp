# omnia-plugin

The `omnia:plugins/loader` capability for the [omnia](https://github.com/augentic/omnia) runtime: a guest names code (package, location, optional sha256 pin) and the host acquires, verifies, and admits it — component bytes never cross the interface, and every trust decision stays host-side.

Everything plugin lives here:

- the loader WIT and the `WasiPlugins` host binding,
- the `Plugins` load path (pin policy, idempotency, acquisition routing) over `omnia-core`'s privileged `Runtime::admit` seam, reachable host-side through `PluginLoader` on `Runtime`,
- the acquisition policy — one slot per location kind — with the built-in `PathMounts` and `RegistryClient` policies, installed by `Plugins::install` from the deployment's `Wiring::extend` hook; `Plugins::install_declared` fills the slots from the deployment's declared locations (the `runtime!` macro's `plugin: { locations: [...] }` list, carried as manifest data).

Depend on the `omnia` facade, not on this crate: the whole surface re-exports there behind `omnia`'s non-default `plugin` feature (`omnia::WasiPlugins`, `omnia::PathMounts`, `omnia::Plugins`, …), including everything a `cache:` store implements — `omnia::ContentStore` and `omnia::ReleaseStore` from here, `omnia::Backend` and `omnia::NoOptions` from the runtime core. A direct dependency on `omnia-plugin` (or on `omnia-core`) is only for building another capability crate of your own.

## License

MIT OR Apache-2.0
