//! Generated `main`: direct-command planning and optional `run` grammar.

mod direct;

use std::env;
use std::process::ExitCode;

pub use self::direct::{MainOptions, ManifestSource};
use crate::{Backends, DeploymentBuilder, Wiring};

/// Entry point for generated `main` functions.
///
/// `options` carries the deployment the `runtime!` macro compiled in: mode
/// and manifest source. Command mode with a compiled-in deployment is a
/// direct command: argv passes to the guest verbatim except the reserved host
/// log flags (`--debug` / `--quiet`), which select the telemetry
/// [`LogMode`](omnia_core::LogMode). Every other shape needs the standard
/// `run [wasm] [--config] -- args…` grammar, served when omnia is built with
/// the `cli` feature.
#[doc(hidden)]
pub async fn main<B, H>(options: MainOptions) -> ExitCode
where
    B: Backends,
    H: Wiring<B>,
{
    let builder = if options.is_direct() {
        match direct::plan(options, env::args_os()) {
            Ok(plan) => plan.into_builder(),
            Err(error) => {
                eprintln!("{error:#}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match cli_plan(options) {
            Ok(builder) => builder,
            #[cfg(feature = "cli")]
            Err(omnia_cli::PlanError::Usage(error)) => error.exit(),
            #[cfg(feature = "cli")]
            Err(omnia_cli::PlanError::Fatal(error)) => {
                eprintln!("{error:#}");
                return ExitCode::FAILURE;
            }
            #[cfg(not(feature = "cli"))]
            Err(error) => {
                eprintln!("{error:#}");
                return ExitCode::FAILURE;
            }
        }
    };
    crate::lifecycle::drive_main::<B, H>(builder).await
}

#[cfg(feature = "cli")]
fn cli_plan(options: MainOptions) -> Result<DeploymentBuilder, omnia_cli::PlanError> {
    materialize(options, env::args_os(), env::var_os("OMNIA_CONFIG"))
}

#[cfg(not(feature = "cli"))]
fn cli_plan(_: MainOptions) -> Result<DeploymentBuilder, anyhow::Error> {
    Err(anyhow::anyhow!(
        "this runtime was built without omnia's `cli` feature; compile the deployment in \
         (command mode with a manifest) or enable the feature"
    ))
}

#[cfg(feature = "cli")]
fn materialize(
    options: MainOptions, argv: impl IntoIterator<Item = std::ffi::OsString>,
    omnia_config: Option<std::ffi::OsString>,
) -> Result<DeploymentBuilder, omnia_cli::PlanError> {
    use omnia_cli::RunSource;

    use crate::{Manifest, Mount};

    let (mode, compiled_in) = options.into_parts();
    let plan = omnia_cli::plan(argv, omnia_config, compiled_in.is_some())?;
    let manifest = match plan.source {
        RunSource::Config(path) => Manifest::from_config(path)?,
        RunSource::Wasm(path) => Manifest::from_wasm(path),
        RunSource::CompiledIn => compiled_in.expect("planner checked").into_manifest()?,
    };
    let mounts = plan.mounts.into_iter().map(|arg| Mount {
        name: arg.name,
        path: arg.host_path,
        writable: arg.writable,
    });
    Ok(DeploymentBuilder::new()
        .manifest(manifest.mounts(mounts).link(plan.link))
        .args(plan.args)
        .mode(mode))
}

#[cfg(all(test, feature = "cli"))]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::*;
    use crate::Mode;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn fatal(error: omnia_cli::PlanError) -> String {
        match error {
            omnia_cli::PlanError::Fatal(error) => format!("{error:#}"),
            omnia_cli::PlanError::Usage(error) => {
                panic!("expected a fatal error, got usage: {error}")
            }
        }
    }

    #[test]
    fn compiled_path_load_failure_surfaces() {
        let options = MainOptions::new(Mode::Server)
            .manifest(ManifestSource::Path(PathBuf::from("/nonexistent/omnia.toml")));
        let error = materialize(options, argv(&["bin", "run"]), None)
            .expect_err("a missing compiled-in manifest path must fail");
        assert!(fatal(error).contains("reading manifest"));
    }
}
