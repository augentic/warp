omnia_host_macros::runtime!({
    plugin: {
        locations: [{ registry: "ghcr.io" }],
        cache: PluginCache,
    },
    guests: [
        { id: "api", source: "api.wasm" },
    ],
});

fn main() {}
