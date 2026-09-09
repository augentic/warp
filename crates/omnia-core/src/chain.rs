//! Per-chain dispatch context and the policy that bounds it.

use std::time::Duration;

use anyhow::{Result, bail};

use crate::RuntimeOptions;
use crate::registry::GuestId;

tokio::task_local! {
    // The context of the dispatch chain the current task is serving. Carried
    // across the in-process carrier via the wRPC accept context and
    // re-established around each served invocation, so concurrent, unrelated
    // chains never share a depth budget or a wall-clock policy.
    static CHAIN_CTX: ChainCtx;
}

/// Per-chain dispatch context: the nesting depth (0 at a chain root) plus
/// whether the chain root runs uncapped (a command-mode `wasi:cli/run` drive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainCtx {
    /// Nesting depth of the current hop (0 at a chain root).
    pub depth: usize,
    /// Whether the chain root runs without the wall-clock cap.
    pub uncapped: bool,
}

/// Run `fut` with the chain context carried over from an incoming dispatch, so
/// nested host-mediated calls made while serving it count against the same
/// chain and inherit its wall-clock policy.
#[cfg(feature = "wrpc")]
pub fn with_chain<F>(ctx: ChainCtx, fut: F) -> impl Future<Output = F::Output>
where
    F: Future,
{
    CHAIN_CTX.scope(ctx, fut)
}

/// Run `fut` as a command-mode chain root: link dispatches it makes (and their
/// nested hops) run without the `GUEST_TIMEOUT_MS` wall-clock cap.
pub fn as_command_chain<F>(fut: F) -> impl Future<Output = F::Output>
where
    F: Future,
{
    CHAIN_CTX.scope(
        ChainCtx {
            depth: 0,
            uncapped: true,
        },
        fut,
    )
}

/// The context of the dispatch chain currently being served (a capped root
/// outside any scope).
fn current_chain() -> ChainCtx {
    CHAIN_CTX.try_with(|ctx| *ctx).unwrap_or_default()
}

/// The deployment-wide bounds on a dispatch chain: its maximum nesting depth
/// and the wall-clock cap on each server-rooted hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPolicy {
    /// Maximum dispatch nesting depth (`MAX_DISPATCH_DEPTH`).
    pub max_depth: usize,
    /// Wall-clock bound on each host-mediated dispatch (`GUEST_TIMEOUT_MS`).
    pub timeout: Duration,
}

impl ChainPolicy {
    /// Enter a dispatch, bounding the current chain's nesting depth; returns
    /// the context the dispatched call runs at (depth plus the inherited
    /// wall-clock policy), to be carried to the serve side.
    ///
    /// Depth is per call chain (A->B->C, each awaited to completion before the
    /// caller returns), so concurrent, unrelated chains never contend for the
    /// same budget.
    ///
    /// # Errors
    ///
    /// Returns an error if the hop would exceed `max_depth`.
    pub fn enter(&self, target: &GuestId) -> Result<ChainCtx> {
        let current = current_chain();
        let depth = current.depth + 1;
        if depth > self.max_depth {
            bail!(
                "link dispatch depth {depth} exceeds maximum {} (target `{target}`); raise \
                 MAX_DISPATCH_DEPTH if this is intentional",
                self.max_depth
            );
        }
        Ok(ChainCtx {
            depth,
            uncapped: current.uncapped,
        })
    }
}

impl From<&RuntimeOptions> for ChainPolicy {
    fn from(options: &RuntimeOptions) -> Self {
        Self {
            max_depth: options.max_dispatch_depth,
            timeout: options.guest_timeout,
        }
    }
}
