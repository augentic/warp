//! WASI CLI glue for guest command entrypoints.

use std::borrow::Cow;
#[cfg(feature = "command")]
use std::ffi::OsString;
use std::fmt;
#[cfg(target_arch = "wasm32")]
use std::future::Future;
use std::io::{ErrorKind, Write};

#[cfg(feature = "command")]
pub use clap_complete::Shell;
use serde::{Serialize, Serializer};

use crate::api::ErrorBody;

/// The exit status of a command-line usage error (`EX_USAGE`).
pub const USAGE_EXIT: u8 = 64;

/// Wires an `async fn` as the guest's `wasi:cli/run` export, driven through
/// `execute_wasi` (wasm32 only) so telemetry is initialized and flushed
/// around it.
///
/// The entry returns `()` (a scenario that asserts internally and traps on
/// failure), `Result<(), u8>` (a CLI reporting an exit status), or a
/// [`Response`] (buffered channels plus exit status); see [`IntoExit`].
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
/// succeeds, `Result<(), u8>` passes its code through, and [`Response`]
/// writes its channels first.
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

/// Buffered command output and exit status.
///
/// Both channels are whole buffers written at the exit boundary; a command
/// that must stream writes to the process channels itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    /// Bytes written to standard output.
    pub stdout: Vec<u8>,

    /// Bytes written to standard error.
    pub stderr: Vec<u8>,

    /// The process exit status.
    pub exit: u8,
}

impl Response {
    /// A successful response carrying stdout.
    #[must_use]
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: Vec::new(),
            exit: 0,
        }
    }

    /// A failed response carrying stderr and its exit status.
    #[must_use]
    pub fn failure(stderr: impl Into<Vec<u8>>, exit: u8) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: stderr.into(),
            exit,
        }
    }

    /// A usage error rendered by clap, exiting [`USAGE_EXIT`].
    #[cfg(feature = "command")]
    #[must_use]
    pub fn usage(error: &clap::Error) -> Self {
        Self::failure(error.render().to_string(), USAGE_EXIT)
    }
}

impl IntoExit for Response {
    fn into_exit(self) -> Result<(), u8> {
        write_channel(&mut std::io::stdout(), &self.stdout)?;
        write_channel(&mut std::io::stderr(), &self.stderr)?;
        if self.exit == 0 { Ok(()) } else { Err(self.exit) }
    }
}

fn write_channel(sink: &mut impl Write, bytes: &[u8]) -> Result<(), u8> {
    match sink.write_all(bytes).and_then(|()| sink.flush()) {
        Ok(()) => Ok(()),
        // A reader that has gone away (`head`, a closed pipe) is the
        // consumer's decision, not this command's failure: keep its own exit.
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        // Any other refused process channel is unclassified, so it takes
        // the exit the one map gives a `ServerError`.
        Err(error) => Err(crate::server_error!("process channel: {error}").exit_code()),
    }
}

/// The failure envelope a command reports: the error, its exit status, and
/// an optional remedy hint.
#[derive(Clone, Debug)]
pub struct Failure {
    error: crate::Error,
    hint: Option<Cow<'static, str>>,
}

impl Failure {
    /// The underlying error.
    #[must_use]
    pub const fn error(&self) -> &crate::Error {
        &self.error
    }

    /// The transport-neutral wire body shared with HTTP.
    #[must_use]
    pub fn body(&self) -> ErrorBody {
        ErrorBody::from(&self.error)
    }

    /// The exit status from the error's exit map.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.error.exit_code()
    }

    /// The remedy hint, when one was attached.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Attach a remedy hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<Cow<'static, str>>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Render the envelope as `error[<code>]: <message>` and an optional
    /// `hint:` line; the text-mode counterpart of its JSON form.
    ///
    /// # Errors
    ///
    /// Propagates the sink's formatting failure.
    pub fn text(&self, out: &mut dyn fmt::Write) -> fmt::Result {
        let ErrorBody { error, message } = self.body();
        writeln!(out, "error[{error}]: {message}")?;
        if let Some(hint) = self.hint() {
            writeln!(out, "hint: {hint}")?;
        }
        Ok(())
    }
}

impl From<crate::Error> for Failure {
    fn from(error: crate::Error) -> Self {
        Self { error, hint: None }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        crate::Error::from(error).into()
    }
}

// The flat wire shape: `{"error","message","exit-code","hint"?}`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct FailureWire<'a> {
    #[serde(flatten)]
    body: ErrorBody,
    exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

impl Serialize for Failure {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        FailureWire {
            body: self.body(),
            exit_code: self.exit_code(),
            hint: self.hint(),
        }
        .serialize(serializer)
    }
}

/// The outcome of parsing argv: the grammar, or one of clap's own complete
/// responses.
#[cfg(feature = "command")]
#[derive(Debug)]
pub enum Parsed<A> {
    /// The parsed grammar.
    App(A),

    /// Help or version text destined for stdout with exit 0.
    Display(String),

    /// A usage error destined for stderr; see [`Response::usage`].
    Usage(clap::Error),
}

/// Parse argv into the grammar `A`, classifying clap's own outcomes.
#[cfg(feature = "command")]
#[must_use]
pub fn parse<A: clap::Parser>(argv: impl IntoIterator<Item: Into<OsString> + Clone>) -> Parsed<A> {
    match A::try_parse_from(argv) {
        Ok(app) => Parsed::App(app),
        Err(error) if error.use_stderr() => Parsed::Usage(error),
        Err(display) => Parsed::Display(display.render().to_string()),
    }
}

/// A shell-completion script for the grammar `A` under the binary `name`.
#[cfg(feature = "command")]
#[must_use]
pub fn completions<A: clap::CommandFactory>(shell: Shell, name: &str) -> Response {
    let mut out = Vec::new();
    clap_complete::generate(shell, &mut A::command(), name, &mut out);
    Response::success(out)
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

#[cfg(test)]
mod tests {
    use std::io::{self, ErrorKind, Write};

    use super::write_channel;

    struct Refusing(ErrorKind);

    impl Write for Refusing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_is_not_a_failure() {
        assert_eq!(write_channel(&mut Refusing(ErrorKind::BrokenPipe), b"body"), Ok(()));
    }

    #[test]
    fn refused_channel_exits_as_server_error() {
        assert_eq!(write_channel(&mut Refusing(ErrorKind::PermissionDenied), b"body"), Err(3));
    }
}
