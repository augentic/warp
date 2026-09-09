omnia_host_macros::runtime!({
    link: ["omnia:shared/log"],
    guests: [
        { id: "api", source: "api.wasm" },
    ],
});

fn main() {}
