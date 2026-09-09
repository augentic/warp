//! Plugin-only: load a host-only handler and exit. The deployment declares
//! no link interfaces, so this guest cannot import `ops`; the host drives
//! the loaded echoer through `Dispatcher` after the run.

#![cfg(target_arch = "wasm32")]

use omnia_guest::plugins::{Location, PluginRef, Plugins as _, WasiPlugins};

omnia_guest::command!(scenario);

async fn scenario() {
    let plugin = WasiPlugins
        .load(
            &PluginRef::builder()
                .package("test:echoer")
                .location(Location::Path("./plugin.wasm".to_owned()))
                .build(),
        )
        .await
        .expect("a host-only handler loads without a declared link interface");
    assert_eq!(plugin.id(), "test:echoer");
}
