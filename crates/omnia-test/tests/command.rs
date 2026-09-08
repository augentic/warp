//! The command façade's exit plumbing through the real runtime: the status a
//! `command!` guest's `Response` carries is the status the host observes.

use omnia_test::host::{Backends, Deployment};
use omnia_wasi_otel::WasiOtel;

test_programs::foreach_command!();

/// Drives the `exit_map` guest once with the operator arguments `args`.
async fn exit_of(args: &[&str]) -> u8 {
    Deployment::new()
        .guest("cli", test_programs::COMMAND_EXIT_MAP)
        .args(args.iter().copied())
        .run(Backends::defaults().await, |deployment| {
            deployment.host::<WasiOtel, Backends>()?;
            Ok(())
        })
        .await
        .expect("deployment runs")
        .code_u8()
}

#[tokio::test]
async fn command_exit_map() {
    assert_eq!(exit_of(&["ok"]).await, 0);
    assert_eq!(exit_of(&["bad"]).await, 1);
    assert_eq!(exit_of(&["missing"]).await, 2);
    assert_eq!(exit_of(&["upstream"]).await, 4);
    assert_eq!(exit_of(&["bogus"]).await, 64, "an unknown verb is a usage error");
    assert_eq!(exit_of(&["--format", "json", "missing"]).await, 2, "the exit ignores the format");
}
