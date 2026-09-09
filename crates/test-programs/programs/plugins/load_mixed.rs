//! Mixed: load a link target and a host-only handler. Ping the target over
//! the declared link; the host then drives the handler (which exports no
//! `ops`) after the run. Both loads succeed — admission does not require a
//! linked export.

#![cfg(target_arch = "wasm32")]

wit_bindgen::generate!({
    world: "caller",
    path: "wit",
    generate_all,
});

use omnia_guest::plugins::{Location, PluginRef, Plugins as _, WasiPlugins};
use omnia_test::link::ops;

omnia_guest::command!(scenario);

fn plugin(package: &str, path: &str) -> PluginRef {
    PluginRef::builder().package(package).location(Location::Path(path.to_owned())).build()
}

async fn scenario() {
    let target =
        WasiPlugins.load(&plugin("echoer", "./plugin.wasm")).await.expect("a link target loads");
    assert_eq!(target.id(), "echoer");

    let answer = ops::ping(target.id(), "hi");
    assert_eq!(answer, "echoer pong: hi");

    let handler = WasiPlugins
        .load(&plugin("test:handler", "./handler.wasm"))
        .await
        .expect("a host-only handler loads");
    assert_eq!(handler.id(), "test:handler");
}
