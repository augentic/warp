//! WASI CLI glue for guest command entrypoints.

#[cfg(target_arch = "wasm32")]
use std::future::Future;
#[cfg(target_arch = "wasm32")]
use std::io::Write as _;

/// Wires an `async fn` as the guest's `wasi:cli/run` export, driven through
/// `execute_wasi` (wasm32 only) so telemetry is initialized and flushed
/// around it.
///
/// The entry returns `()` (a scenario that asserts internally and traps on
/// failure) or `Result<(), u8>` (a CLI reporting an exit status); see
/// [`IntoExit`].
///
/// ```rust,ignore
/// omnia_guest::command!(main);
///
/// async fn main() -> Result<(), u8> {
///     println!("hello");
///     Ok(())
/// }
/// ```
#[macro_export]
macro_rules! command {
    ($entry:path) => {
        struct CliGuest;

        $crate::wasip3::cli::command::export!(CliGuest);

        impl $crate::wasip3::exports::cli::run::Guest for CliGuest {
            async fn run() -> ::core::result::Result<(), ()> {
                $crate::api::command::execute_wasi(async {
                    $crate::api::command::IntoExit::into_exit($entry().await)
                })
                .await
            }
        }
    };
}

/// The exit status a [`command!`](crate::command) entry yields: `()` always
/// succeeds and `Result<(), u8>` passes its code through.
pub trait IntoExit {
    /// The status handed to the WASI CLI boundary.
    ///
    /// # Errors
    ///
    /// Returns the non-zero exit code the guest reports.
    fn into_exit(self) -> Result<(), u8>;
}

impl IntoExit for () {
    fn into_exit(self) -> Result<(), u8> {
        Ok(())
    }
}

impl IntoExit for Result<(), u8> {
    fn into_exit(self) -> Result<(), u8> {
        self
    }
}

/// Execute a guest command at the WASI CLI boundary.
///
/// Initializes guest telemetry, awaits `run`, and flushes telemetry and
/// stdout. The guest writes its own output; a non-zero status then exits
/// with that exact code.
///
/// # Errors
///
/// Returns `Ok(())` when `run` succeeds. A non-zero status is reported
/// through `wasi:cli/exit` and does not return.
#[cfg(target_arch = "wasm32")]
#[expect(clippy::result_unit_err, reason = "matches the wasi:cli/run contract")]
pub async fn execute_wasi(run: impl Future<Output = Result<(), u8>>) -> Result<(), ()> {
    let guard = omnia_wasi_otel::init();
    let result = run.await;
    // `exit-with-code` does not return (analogous to a trap), so no
    // `Drop` runs past it: flush telemetry as soon as the run completes.
    omnia_wasi_otel::flush_guard(guard).await;
    // Rust's exit-time stdout flush never runs here (`exit-with-code` traps
    // out, and a reactor export has no `main`), so flush the line buffer
    // explicitly; stderr is unbuffered by contract.
    let _ = std::io::stdout().flush();
    if let Err(code) = result
        && code != 0
    {
        wasip3::cli::exit::exit_with_code(code);
    }
    Ok(())
}
