//! Entry planning for the `run` grammar: process argv and environment resolve
//! into a [`RunPlan`] over paths and strings.
//!
//! [`plan`] is pure with respect to the process — argv and `OMNIA_CONFIG` are
//! parameters — so source precedence is unit-testable without spawning a
//! binary.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::anyhow;
use clap::Parser as _;

use crate::cli::{Cli, Command, MountArg};

/// Why entry planning stopped before a run plan could be produced.
pub enum PlanError {
    /// A clap-level outcome (usage error, `--help`, `--version`); the caller
    /// delegates to [`clap::Error::exit`] so stream and exit code match the
    /// standard CLI behavior.
    Usage(clap::Error),
    /// A startup failure reported on stderr.
    Fatal(anyhow::Error),
}

impl From<anyhow::Error> for PlanError {
    fn from(error: anyhow::Error) -> Self {
        Self::Fatal(error)
    }
}

/// Where the deployment comes from — the `--config` › `OMNIA_CONFIG` › `<wasm>`
/// › compiled-in ladder, decided over plain data.
#[derive(Debug, PartialEq, Eq)]
pub enum RunSource {
    /// A `--config` / `OMNIA_CONFIG` manifest path.
    Config(PathBuf),
    /// A positional wasm path.
    Wasm(PathBuf),
    /// The generated `main`'s compiled-in manifest, not loaded here.
    CompiledIn,
}

/// The planner's outcome: source, CLI mounts, link interfaces, and guest argv.
#[derive(Debug, PartialEq, Eq)]
pub struct RunPlan {
    /// Which source the precedence ladder selected.
    pub source: RunSource,
    /// `--mount` arguments, in argv order.
    pub mounts: Vec<MountArg>,
    /// `--link` arguments, in argv order.
    pub link: Vec<String>,
    /// Arguments forwarded to the guest as its argv (everything after `--`).
    pub args: Vec<String>,
}

/// Plan the standard `run [wasm] [--config] -- args…` grammar, resolving the
/// source by the `--config` › `OMNIA_CONFIG` › positional wasm › compiled-in
/// ladder.
///
/// `has_compiled_in` is whether the generated `main` compiled a manifest in;
/// this function does not load it.
///
/// # Errors
///
/// Returns [`PlanError::Usage`] when clap rejects argv, or [`PlanError::Fatal`]
/// when no source is available or the subcommand is not `run`.
pub fn plan(
    argv: impl IntoIterator<Item = OsString>, omnia_config: Option<OsString>, has_compiled_in: bool,
) -> Result<RunPlan, PlanError> {
    let cli = Cli::try_parse_from(argv).map_err(PlanError::Usage)?;
    match cli.command {
        Command::Run {
            wasm,
            config,
            mounts,
            link,
            args,
        } => {
            let config = config.or_else(|| omnia_config.map(PathBuf::from));
            let source = match (config, wasm) {
                (Some(config), _) => RunSource::Config(config),
                (None, Some(wasm)) => RunSource::Wasm(wasm),
                (None, None) if has_compiled_in => RunSource::CompiledIn,
                (None, None) => {
                    return Err(PlanError::Fatal(anyhow!(
                        "no guest specified: pass a <wasm> path, or --config <omnia.toml> (or \
                         set OMNIA_CONFIG)"
                    )));
                }
            };
            Ok(RunPlan {
                source,
                mounts,
                link,
                args,
            })
        }
        #[cfg(feature = "jit")]
        Command::Compile { .. } => Err(PlanError::Fatal(anyhow!(
            "the generated `main` only supports `run`; supply a custom `main` for other subcommands"
        ))),
    }
}

// Unit tests by design: `plan` is factored pure (argv and `OMNIA_CONFIG` are
// parameters) precisely so source precedence is testable without spawning a
// binary.
#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn fatal(error: PlanError) -> String {
        match error {
            PlanError::Fatal(error) => format!("{error:#}"),
            PlanError::Usage(error) => panic!("expected a fatal error, got usage: {error}"),
        }
    }

    #[test]
    fn config_beats_positional_wasm_and_compiled_source() {
        let plan = plan(argv(&["bin", "run", "guest.wasm", "--config", "omnia.toml"]), None, true)
            .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.source, RunSource::Config(PathBuf::from("omnia.toml")));
    }

    #[test]
    fn omnia_config_env_beats_positional_wasm() {
        let plan =
            plan(argv(&["bin", "run", "guest.wasm"]), Some(OsString::from("from_env.toml")), false)
                .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.source, RunSource::Config(PathBuf::from("from_env.toml")));
    }

    #[test]
    fn positional_wasm_beats_compiled_source() {
        let plan = plan(argv(&["bin", "run", "guest.wasm"]), None, true)
            .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.source, RunSource::Wasm(PathBuf::from("guest.wasm")));
    }

    #[test]
    fn compiled_source_is_the_fallback() {
        let plan = plan(argv(&["bin", "run"]), None, true)
            .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.source, RunSource::CompiledIn);
    }

    #[test]
    fn no_source_fails() {
        let error = plan(argv(&["bin", "run"]), None, false)
            .expect_err("a sourceless deployment must fail");
        assert!(fatal(error).contains("no guest specified"));
    }

    #[test]
    fn link_flag_is_collected() {
        let plan =
            plan(argv(&["bin", "run", "guest.wasm", "--link", "omnia:link/echo"]), None, false)
                .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.link, ["omnia:link/echo"]);
    }

    #[test]
    fn command_mode_without_deployment_keeps_run_grammar() {
        let plan = plan(argv(&["bin", "run", "guest.wasm"]), None, false)
            .unwrap_or_else(|error| panic!("{}", fatal(error)));
        assert_eq!(plan.source, RunSource::Wasm(PathBuf::from("guest.wasm")));
    }

    #[test]
    fn usage_error_is_delegated_to_clap() {
        let error = plan(argv(&["bin", "bogus"]), None, false)
            .expect_err("an unknown subcommand is a usage error");
        assert!(matches!(error, PlanError::Usage(_)));
    }
}
