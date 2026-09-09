# omnia-cli

The `run` command-line grammar for [omnia](https://github.com/augentic/omnia) runtime binaries: `run [wasm] [--config <omnia.toml>] [--mount …] [--link …] -- args…`, with the deployment manifest resolved by the `--config` › `OMNIA_CONFIG` › positional wasm › compiled-in ladder. The `omnia` facade's `cli` feature selects this crate; it decides the `run` source over paths and strings and returns a `RunPlan`. `omnia` materializes that plan into a `Manifest` / `DeploymentBuilder` and drives the runtime. Direct-command binaries skip this crate: `omnia` peels the reserved host log flags and forwards the rest of argv to the guest.

Depend on the `omnia` facade, not on this crate: `omnia::Cli`, `omnia::Command`, and `omnia::Parser` re-export from here behind the `cli` feature, and the `runtime!` macro's generated `main` reaches this crate through `omnia::main`. A direct dependency on `omnia-cli` (or on `omnia-core`) is never needed by a deployment.

## License

MIT OR Apache-2.0
