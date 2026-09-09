//! # Guest selector
//!
//! When a guest calls a host-mediated import, the host must decide *which*
//! registered guest serves the call. That decision is a consumer-policy detail
//! the runtime core must not hardcode, so it exposes a pluggable [`GuestSelector`]
//! strategy and ships one default.
//!
//! The strategy runs on the typed call (interface, function, and the decoded
//! [`Val`] parameters) *before* the invocation is encoded onto the wRPC carrier.
//! It returns the chosen [`GuestId`] and the parameter list to forward — so a
//! strategy may strip the identity argument or pass it through.

use std::borrow::Cow;

use anyhow::{Result, bail};
use wasmtime::component::Val;

use crate::registry::GuestId;

/// Chooses the target guest for a host-mediated call and the parameters to
/// forward to it.
///
/// The runtime core stays domain-agnostic: an implementation sees only the opaque
/// interface/function names and the typed parameters, and returns an opaque
/// [`GuestId`]. It never parses a consumer scheme.
pub trait GuestSelector: Send + Sync + 'static {
    /// Select the target guest for a call to `func` on `interface`, given its
    /// decoded `params`, returning the chosen identity and the parameters to
    /// forward to the target's matching export.
    ///
    /// A strategy that forwards the parameters unchanged returns
    /// `Cow::Borrowed(params)`; only one that reshapes them pays for a copy.
    ///
    /// # Errors
    ///
    /// Returns an error if no target identity can be derived from the call
    /// (e.g. the strategy expects a leading identity argument that is missing or
    /// not a string).
    fn select<'a>(
        &self, interface: &str, func: &str, params: &'a [Val],
    ) -> Result<(GuestId, Cow<'a, [Val]>)>;
}

/// The default strategy: the first parameter is a string identity.
///
/// The leading argument names the target and is **forwarded through** (the
/// adapter imports and exports the same interface, so the target's export
/// expects the identity argument too). A consumer that wants different behaviour
/// (strip the id, read it from a later argument, key off the interface) supplies
/// its own [`GuestSelector`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FirstArgSelector;

impl GuestSelector for FirstArgSelector {
    fn select<'a>(
        &self, interface: &str, func: &str, params: &'a [Val],
    ) -> Result<(GuestId, Cow<'a, [Val]>)> {
        let Some(Val::String(id)) = params.first() else {
            bail!(
                "selector: call to `{interface}/{func}` must carry a leading string identity \
                 argument, found {:?}",
                params.first()
            );
        };
        Ok((GuestId::from(id.as_str()), Cow::Borrowed(params)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_arg() {
        let params = vec![Val::String("responder".to_owned()), Val::String("hello".to_owned())];
        let (id, forwarded) =
            FirstArgSelector.select("omnia:link/echo", "echo", &params).expect("should select");

        assert_eq!(id, GuestId::from("responder"));
        // The default forwards every parameter (including the identity) through.
        assert_eq!(&*forwarded, params.as_slice());
    }

    #[test]
    fn first_arg_invalid() {
        // A non-string leading argument is rejected.
        let error = FirstArgSelector
            .select("omnia:link/echo", "echo", &[Val::U32(7)])
            .expect_err("a non-string identity must fail");
        assert!(error.to_string().contains("leading string identity"));

        // An empty parameter list is rejected too.
        FirstArgSelector
            .select("omnia:link/echo", "echo", &[])
            .expect_err("a missing identity must fail");
    }
}
