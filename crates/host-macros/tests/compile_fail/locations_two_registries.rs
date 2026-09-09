omnia_host_macros::runtime!({
    plugin: {
        locations: [
            { registry: "ghcr.io" },
            { registry: "registry.example" },
        ],
    },
    guests: [
        { id: "api", source: "api.wasm" },
    ],
});

fn main() {}
