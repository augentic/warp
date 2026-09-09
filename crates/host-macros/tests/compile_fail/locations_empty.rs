omnia_host_macros::runtime!({
    plugin: {
        locations: [],
    },
    guests: [
        { id: "api", source: "api.wasm" },
    ],
});

fn main() {}
