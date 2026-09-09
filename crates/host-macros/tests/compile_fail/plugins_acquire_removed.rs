omnia_host_macros::runtime!({
    plugin: {
        acquire: MountAcquire,
    },
    guests: [
        { id: "api", source: "api.wasm" },
    ],
});

fn main() {}
