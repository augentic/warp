//! Errors

use http::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Result type used across the crate.
pub type Result<T> = anyhow::Result<T, Error>;

/// Domain level error type returned by the adapter.
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    /// Request payload is invalid or missing required fields.
    #[error("code: {code}, description: {description}")]
    BadRequest {
        /// The error code.
        code: String,
        /// The error description.
        description: String,
    },

    /// Resource or data not found.
    #[error("code: {code}, description: {description}")]
    NotFound {
        /// The error code.
        code: String,
        /// The error description.
        description: String,
    },

    /// A non recoverable internal error occurred.
    #[error("code: {code}, description: {description}")]
    ServerError {
        /// The error code.
        code: String,
        /// The error description.
        description: String,
    },

    /// An upstream dependency failed while fulfilling the request.
    #[error("code: {code}, description: {description}")]
    BadGateway {
        /// The error code.
        code: String,
        /// The error description.
        description: String,
    },
}

impl Error {
    /// Returns the HTTP status code associated with the variant.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest { .. } => StatusCode::BAD_REQUEST,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::ServerError { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadGateway { .. } => StatusCode::BAD_GATEWAY,
        }
    }

    /// Returns the process exit code associated with the variant.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::BadRequest { .. } => 1,
            Self::NotFound { .. } => 2,
            Self::ServerError { .. } => 3,
            Self::BadGateway { .. } => 4,
        }
    }

    /// Returns the error code for the variant.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::BadRequest { code, .. }
            | Self::NotFound { code, .. }
            | Self::ServerError { code, .. }
            | Self::BadGateway { code, .. } => code.clone(),
        }
    }

    /// Returns the error description.
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::BadRequest { description, .. }
            | Self::NotFound { description, .. }
            | Self::ServerError { description, .. }
            | Self::BadGateway { description, .. } => description.clone(),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>().join(": ");

        // if type is Error, return it with the newly added context
        if let Some(inner) = err.downcast_ref::<Self>() {
            tracing::debug!("Error: {err}, caused by: {inner}");

            return match inner {
                Self::BadRequest { code, .. } => Self::BadRequest {
                    code: code.clone(),
                    description: chain,
                },
                Self::NotFound { code, .. } => Self::NotFound {
                    code: code.clone(),
                    description: chain,
                },
                Self::ServerError { code, .. } => Self::ServerError {
                    code: code.clone(),
                    description: chain,
                },
                Self::BadGateway { code, .. } => Self::BadGateway {
                    code: code.clone(),
                    description: chain,
                },
            };
        }

        // otherwise, return an Internal error
        Self::ServerError {
            code: "server_error".to_string(),
            description: chain,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::BadRequest {
            code: "serde_json".to_string(),
            description: err.to_string(),
        }
    }
}

/// Create a new `BadRequest` error.
#[macro_export]
macro_rules! bad_request {
    // A lone format string is also an expr; match the literal first so
    // implicit `{name}` captures reach `format!` instead of `.to_string()`.
    ($fmt:literal $($arg:tt)*) => {
        $crate::Error::BadRequest { code: "bad_request".to_string(), description: format!($fmt $($arg)*) }
    };
    ($err:expr $(,)?) => {
        $crate::Error::BadRequest { code: "bad_request".to_string(), description: $err.to_string() }
    };
}

/// Create a new `NotFound` error.
#[macro_export]
macro_rules! not_found {
    ($fmt:literal $($arg:tt)*) => {
        $crate::Error::NotFound { code: "not_found".to_string(), description: format!($fmt $($arg)*) }
    };
    ($err:expr $(,)?) => {
        $crate::Error::NotFound { code: "not_found".to_string(), description: $err.to_string() }
    };
}

/// Create a new `ServerError` error.
#[macro_export]
macro_rules! server_error {
    ($fmt:literal $($arg:tt)*) => {
        $crate::Error::ServerError { code: "server_error".to_string(), description: format!($fmt $($arg)*) }
    };
    ($err:expr $(,)?) => {
        $crate::Error::ServerError { code: "server_error".to_string(), description: $err.to_string() }
    };
}

/// Create a new `BadGateway` error.
#[macro_export]
macro_rules! bad_gateway {
    ($fmt:literal $($arg:tt)*) => {
        $crate::Error::BadGateway { code: "bad_gateway".to_string(), description: format!($fmt $($arg)*) }
    };
    ($err:expr $(,)?) => {
        $crate::Error::BadGateway { code: "bad_gateway".to_string(), description: $err.to_string() }
    };
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result, anyhow};
    use serde_json::Value;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Registry, fmt};

    use super::Error;

    #[test]
    fn with_context() {
        Registry::default().with(EnvFilter::new("debug")).with(fmt::layer()).init();

        let context_error = || -> Result<(), Error> {
            Err(bad_request!("invalid input"))
                .context("doing something")
                .context("more context")?;
            Ok(())
        };

        let result = context_error();
        assert_eq!(
            result.unwrap_err().to_string(),
            bad_request!(
                "more context: doing something: code: bad_request, description: invalid input"
            )
            .to_string()
        );
    }

    // Test that error details are returned as json.
    #[test]
    fn r9k_context() {
        let result = Err::<(), Error>(server_error!("server error")).context("request context");
        let err: Error = result.unwrap_err().into();

        assert_eq!(
            err.to_string(),
            "code: server_error, description: request context: code: server_error, description: server error"
        );
    }

    #[test]
    fn anyhow_context() {
        let result = Err::<(), anyhow::Error>(anyhow!("one-off error")).context("error context");
        let err: Error = result.unwrap_err().into();

        assert_eq!(
            err.to_string(),
            "code: server_error, description: error context: one-off error"
        );
    }

    #[test]
    fn serde_context() {
        let result: Result<Value, anyhow::Error> =
            serde_json::from_str(r#"{"foo": "bar""#).context("error context");
        let err: Error = result.unwrap_err().into();

        assert_eq!(
            err.to_string(),
            "code: server_error, description: error context: EOF while parsing an object at line 1 column 13"
        );
    }

    #[test]
    fn shortcut_macros_format() {
        let field = "name";
        assert_eq!(bad_request!("invalid field: {field}").description(), "invalid field: name");
        assert_eq!(not_found!("missing {field}").description(), "missing name");
        assert_eq!(server_error!("failed {field}").description(), "failed name");
        assert_eq!(bad_gateway!("upstream {field}").description(), "upstream name");

        let err = bad_request!("invalid field: {field}", field = "other");
        assert_eq!(err.code(), "bad_request");
        assert_eq!(err.description(), "invalid field: other");

        let id = "abc";
        let name = "spec.md";
        let reason = "invalid utf-8";
        assert_eq!(
            server_error!("revision `{id}` contains `{name}` but it is not UTF-8 ({reason})")
                .description(),
            "revision `abc` contains `spec.md` but it is not UTF-8 (invalid utf-8)"
        );
        assert_eq!(
            server_error!("revision `{}` contains `{}` but it is not UTF-8 ({})", id, name, reason)
                .description(),
            "revision `abc` contains `spec.md` but it is not UTF-8 (invalid utf-8)"
        );
    }

    #[test]
    fn exit_map() {
        assert_eq!(bad_request!("x").exit_code(), 1);
        assert_eq!(not_found!("x").exit_code(), 2);
        assert_eq!(server_error!("x").exit_code(), 3);
        assert_eq!(bad_gateway!("x").exit_code(), 4);
    }

    #[test]
    fn shortcut_macros_display() {
        let msg = String::from("missing widget");
        let err = not_found!(msg);
        assert_eq!(err.code(), "not_found");
        assert_eq!(err.description(), "missing widget");

        let cause = anyhow!("upstream timeout");
        let err = bad_gateway!(cause);
        assert_eq!(err.code(), "bad_gateway");
        assert_eq!(err.description(), "upstream timeout");
    }
}
