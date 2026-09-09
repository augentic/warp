//! The build-script environment: cargo's variables, the nested command, and
//! the sanitising that keeps outer host flags out of the wasm32 build.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) const WASM_TARGET: &str = "wasm32-wasip2";

/// A cargo-provided variable, or a panic naming it: the pipeline only runs
/// under `cargo` as a build script.
pub(super) fn var(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| panic!("`{name}` is set by cargo for build scripts"))
}

pub(super) fn path_var(name: &str) -> PathBuf {
    PathBuf::from(var(name))
}

/// Whether the outer build targets `wasm32`, where nesting a fixture build
/// would recurse.
pub(super) fn outer_target_is_wasm32() -> bool {
    env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
}

/// Whether the outer `RUSTFLAGS` deny warnings, the one flag propagated into
/// the nested build.
pub(super) fn denies_warnings(rustflags: Option<&str>) -> bool {
    rustflags.is_some_and(|flags| flags.contains("-Dwarnings") || flags.contains("-D warnings"))
}

/// The nested `cargo build` for the fixture components, sanitised and pointed
/// at its own target directory.
pub(super) fn nested_build(root: &Path, target_dir: &Path) -> Command {
    // Reuse the outer cargo to stay on its toolchain; read before sanitising.
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let deny_warnings = denies_warnings(env::var("RUSTFLAGS").ok().as_deref());

    let mut command = Command::new(cargo);
    command.current_dir(root).args(["build", "--locked", "--target", WASM_TARGET]);
    sanitise(&mut command, env::vars_os().filter_map(|(key, _)| key.into_string().ok()));
    command.env("CARGO_TARGET_DIR", target_dir);
    if deny_warnings {
        command.env("RUSTFLAGS", "-Dwarnings");
    }
    command
}

/// Strips the outer build's `CARGO_*` and `RUST*` variables from `command`
/// so host flags do not leak into the wasm32 build, keeping only the settings
/// cargo itself needs (`CARGO_HOME`, offline mode). Returns what was removed.
fn sanitise(command: &mut Command, keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut removed: Vec<String> = keys.into_iter().filter(|key| should_strip(key)).collect();
    // The outer command may be `cargo clippy`, whose workspace wrapper would
    // run clippy-driver over the guests' wasm32 dep tree; the fixtures are a
    // plain rustc build. The wrappers are removed even when unset so the
    // nested build never inherits them from a later `env`.
    for key in STRIPPED_TOOLCHAIN_VARS {
        if !removed.iter().any(|removed| removed == key) {
            removed.push((*key).to_owned());
        }
    }
    removed.sort();
    for key in &removed {
        command.env_remove(key);
    }
    removed
}

const KEPT_CARGO_VARS: [&str; 2] = ["CARGO_HOME", "CARGO_NET_OFFLINE"];
const STRIPPED_TOOLCHAIN_VARS: [&str; 5] =
    ["RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"];

fn should_strip(key: &str) -> bool {
    (key.starts_with("CARGO_") && !KEPT_CARGO_VARS.contains(&key))
        || STRIPPED_TOOLCHAIN_VARS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_warnings() {
        assert!(denies_warnings(Some("-Dwarnings")));
        assert!(denies_warnings(Some("-C target-cpu=native -D warnings")));
        assert!(!denies_warnings(Some("-C target-cpu=native")));
        assert!(!denies_warnings(None));
    }

    #[test]
    fn sanitise_env() {
        let outer = [
            "CARGO_HOME",
            "CARGO_NET_OFFLINE",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_MANIFEST_DIR",
            "CARGO_CFG_TARGET_ARCH",
            "RUSTC_WORKSPACE_WRAPPER",
            "PATH",
            "HOME",
        ];
        let removed = sanitise(&mut Command::new("true"), outer.map(str::to_owned));
        assert_eq!(
            removed,
            [
                "CARGO_CFG_TARGET_ARCH",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_MANIFEST_DIR",
                "RUSTC",
                "RUSTC_WORKSPACE_WRAPPER",
                "RUSTC_WRAPPER",
                "RUSTDOCFLAGS",
                "RUSTFLAGS",
            ]
        );
    }
}
