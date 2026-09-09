//! A component exporting no declared link interface still loads: admission
//! no longer requires a linked export, because such a guest remains reachable
//! through the host `Dispatcher`. Dispatching to it over the link instead
//! fails at the call site — the polyfill traps this caller, so the host side
//! asserts the run fails after the load succeeded.

#![cfg(target_arch = "wasm32")]

wit_bindgen::generate!({
    world: "caller",
    path: "wit",
    generate_all,
});

use omnia_guest::plugins::{Location, PluginRef, Plugins as _, WasiPlugins};
use omnia_test::link::ops;

omnia_guest::command!(scenario);

async fn scenario() {
    let unlinked = PluginRef::builder()
        .package("test:unlinked")
        .location(Location::Path("./noseam.wasm".to_owned()))
        .build();
    let plugin = WasiPlugins.load(&unlinked).await.expect("an unlinked component loads");
    assert_eq!(plugin.id(), "test:unlinked");

    // Never returns: the target serves no `ops`, so the link call traps.
    let answer = ops::ping(plugin.id(), "hi");
    panic!("a link call to an unlinked guest must not answer: {answer}");
}
