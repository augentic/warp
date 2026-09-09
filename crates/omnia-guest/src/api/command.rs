//! The command façade: argv in, buffered channels and an exit status out.
//!
//! The command-line mirror of [`api::http`](crate::api::http) over the same
//! [`Client`]: [`parse`] classifies argv into the grammar or one of clap's
//! own complete responses, and [`Command`] projects each verb — decode →
//! [`Client::call`] → encode — onto a [`Response`]. A success body rides
//! stdout in the selected [`Format`]; a [`Failure`] envelope rides stderr
//! with the exit status from [`Error::exit_code`](crate::Error::exit_code),
//! and a clap usage error exits [`USAGE_EXIT`]. [`command!`](crate::command)
//! binds an `async fn main() -> Response` as the `wasi:cli/run` export and
//! [`IntoExit`] writes the channels at that boundary.
//!
//! ```rust,ignore
//! use omnia_guest::api::command::{Command, Parsed, Response, parse};
//! use omnia_guest::api::{Client, Metadata};
//!
//! omnia_guest::command!(main);
//!
//! async fn main() -> Response {
//!     let app = match parse::<App>(wasip3::cli::environment::get_arguments()) {
//!         Parsed::App(app) => app,
//!         Parsed::Display(text) => return Response::success(text),
//!         Parsed::Usage(error) => return Response::usage(&error),
//!     };
//!     let client = Client::new("app", Provider);
//!     let metadata = Metadata::from_env("APP");
//!     let command = Command::new(&client, &metadata, app.format);
//!     match app.verb {
//!         Verb::Greet { name } => command.call(greet, || Ok(Greet { name }), render_greeting).await,
//!     }
//! }
//! ```

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

use crate::api::{Client, ErrorBody, Format, Handler, Metadata};

/// The exit status of a command-line usage error (`EX_USAGE`).
pub const USAGE_EXIT: u8 = 64;

impl Metadata {
    /// Build metadata from the process environment: `<PREFIX>_REQUEST_ID`,
    /// `<PREFIX>_CORRELATION_ID`, and `<PREFIX>_CAUSATION_ID`.
    ///
    /// The command line's carrier for the ids HTTP reads from headers; a
    /// missing request id is minted as on every transport.
    #[must_use]
    pub fn from_env(prefix: &str) -> Self {
        Self::from_lookup(|name| std::env::var(env_key(prefix, name)).ok())
    }
}

// The environment variable carrying the metadata field `name` under `prefix`.
fn env_key(prefix: &str, name: &str) -> String {
    format!("{prefix}_{}", name.to_ascii_uppercase().replace('-', "_"))
}

/// Wires an `async fn` as the guest's `wasi:cli/run` export, driven through
/// `execute_wasi` (wasm32 only) so telemetry is initialized and flushed
/// around it.
///
/// The entry returns a [`Response`] (buffered channels plus exit status, the
/// façade's shape), `Result<(), u8>` (a CLI that writes its own channels and
/// reports an exit status), or `()` (a scenario that asserts internally and
/// traps on failure); see [`IntoExit`].
///
/// ```rust,ignore
/// use omnia_guest::api::command::{Parsed, Response, parse};
///
/// omnia_guest::command!(main);
///
/// async fn main() -> Response {
///     match parse::<App>(wasip3::cli::environment::get_arguments()) {
///         Parsed::App(app) => dispatch(app).await,
///         Parsed::Display(text) => Response::success(text),
///         Parsed::Usage(error) => Response::usage(&error),
///     }
/// }
/// ```
#[macro_export]
macro_rules! command {
    ($entry:path) => {
        struct CliGuest;

        $crate::wasip3::cli::command::export!(CliGuest);

        impl $crate::wasip3::exports::cli::run::Guest for CliGuest {
            async fn run() -> ::core::result::Result<(), ()> {
                $crate::api::command::execute_wasi($entry()).await;
                Ok(())
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

type HintFn<'a> = Box<dyn Fn(&crate::Error) -> Option<Cow<'static, str>> + Send + Sync + 'a>;

/// The command projector: decode → [`Client::call`] → encode onto a
/// [`Response`], with the [`Failure`] envelope on stderr.
pub struct Command<'a, P> {
    client: &'a Client<P>,
    metadata: &'a Metadata,
    format: Format,
    hint: Option<HintFn<'a>>,
}

impl<'a, P: Send + Sync + 'static> Command<'a, P> {
    /// Project verbs over `client` with `metadata`, encoding bodies as
    /// `format`.
    #[must_use]
    pub const fn new(client: &'a Client<P>, metadata: &'a Metadata, format: Format) -> Self {
        Self {
            client,
            metadata,
            format,
            hint: None,
        }
    }

    /// Attach a remedy-hint fn consulted for every failure that carries no
    /// hint of its own.
    #[must_use]
    pub fn hints(
        mut self, hint: impl Fn(&crate::Error) -> Option<Cow<'static, str>> + Send + Sync + 'a,
    ) -> Self {
        self.hint = Some(Box::new(hint));
        self
    }

    /// Run one verb: `decode` the input, invoke `handler` through the
    /// client, and encode the output (`render` is its text form).
    pub async fn call<F, I, D, R>(&self, handler: F, decode: D, render: R) -> Response
    where
        F: Handler<P, I>,
        F::Output: Serialize,
        F::Error: Into<Failure>,
        D: FnOnce() -> Result<I, crate::Error>,
        R: Fn(&F::Output, &mut dyn fmt::Write) -> fmt::Result,
    {
        let input = match decode() {
            Ok(input) => input,
            Err(error) => return self.refuse(Failure::from(error)),
        };
        match self.client.call(handler, input, self.metadata).await {
            Ok(output) => Response::success(self.format.encode(&output, render).bytes),
            Err(error) => self.refuse(error.into()),
        }
    }

    fn refuse(&self, mut failure: Failure) -> Response {
        if failure.hint.is_none()
            && let Some(hint) = &self.hint
        {
            failure.hint = hint(&failure.error);
        }
        Response::failure(self.format.encode(&failure, Failure::text).bytes, failure.exit_code())
    }
}

impl<P> fmt::Debug for Command<'_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("format", &self.format)
            .field("hint", &self.hint.is_some())
            .finish_non_exhaustive()
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

/// Execute a guest command entry at the WASI CLI boundary.
///
/// Initializes guest telemetry, awaits `entry`, and flushes telemetry and
/// stdout. The entry yields any [`IntoExit`] outcome; a non-zero status then
/// exits with that exact code through `wasi:cli/exit` and does not return.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub async fn execute_wasi<E: IntoExit>(entry: impl Future<Output = E>) {
    let guard = omnia_wasi_otel::init();
    let result = entry.await.into_exit();
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
}

#[cfg(test)]
mod tests {
    use std::io::{self, ErrorKind, Write};

    use super::{env_key, write_channel};

    #[test]
    fn env_key_prefix() {
        assert_eq!(env_key("CLI", "request-id"), "CLI_REQUEST_ID");
        assert_eq!(env_key("CLI", "correlation-id"), "CLI_CORRELATION_ID");
        assert_eq!(env_key("EXIT_MAP", "causation-id"), "EXIT_MAP_CAUSATION_ID");
    }

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
    fn broken_pipe() {
        assert_eq!(write_channel(&mut Refusing(ErrorKind::BrokenPipe), b"body"), Ok(()));
    }

    #[test]
    fn refused_channel() {
        assert_eq!(write_channel(&mut Refusing(ErrorKind::PermissionDenied), b"body"), Err(3));
    }
}
