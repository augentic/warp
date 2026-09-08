//! Shared dispatch state: selector, dispatch interfaces, transport, and chain policy.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use super::selector::GuestSelector;
use super::transport::InProcess;
use crate::chain::ChainPolicy;

/// The long-lived dispatch state shared by every polyfilled import.
///
/// It carries the selector strategy, the union of host-mediated interfaces, the
/// bound transport carrier, and the chain policy (depth and wall-clock bounds).
pub struct DispatchHandle {
    pub(super) selector: Arc<dyn GuestSelector>,
    links: BTreeSet<Box<str>>,
    transport: InProcess,
    pub(super) policy: ChainPolicy,
}

impl DispatchHandle {
    /// Create a shared dispatch handle. The transport carrier starts empty;
    /// [`super::serve_links`] (via `Deployment::assemble`) populates it with
    /// each target's serve side.
    #[must_use]
    pub fn new(
        selector: Arc<dyn GuestSelector>, links: BTreeSet<Box<str>>, max_depth: usize,
        timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            selector,
            links,
            transport: InProcess::default(),
            policy: ChainPolicy { max_depth, timeout },
        })
    }

    /// The deployment's host-mediated link interface names (the manifest
    /// `plugins` list) — the set of
    /// interfaces to polyfill (caller side) and serve (callee side).
    #[must_use]
    pub const fn links(&self) -> &BTreeSet<Box<str>> {
        &self.links
    }

    /// The bound transport carrier.
    pub(crate) const fn transport(&self) -> &InProcess {
        &self.transport
    }
}
