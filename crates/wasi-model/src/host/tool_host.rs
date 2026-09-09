use std::fmt::Debug;

pub use omnia_core::FutureResult;
use serde::{Deserialize, Serialize};

/// Host-side capabilities for one completion, lent to backends that need them.
pub trait ToolHost: Debug + Send + Sync {
    /// Run one declared function tool through the completion's session: the
    /// guest's tool closure answers. The outer error is a hard host failure
    /// (undeclared tool, exhausted budget, closed session, oversize result,
    /// timeout); the inner `Err` is the tool's own model-visible failure
    /// text, fed back to the model as repairable content.
    fn call_tool(&self, name: String, arguments: String) -> FutureResult<ToolOutcome>;

    /// Ask the guest whether `candidate` is the answer. `Ok(())` finishes
    /// the completion; `Err(text)` is the correction turn the backend appends
    /// verbatim. Routed over the session's `calls` stream as the reserved
    /// tool `check`; `tool_timeout` applies, the tool-call budget does not.
    /// The outer error is a hard host failure (closed session, timeout).
    fn check(&self, candidate: String) -> FutureResult<Result<(), String>>;

    /// Bounded workspace read via the lent `wasi:filesystem` capability.
    fn read(&self, path: String) -> FutureResult<Vec<u8>>;

    /// Bounded workspace listing via the lent `wasi:filesystem` capability.
    fn list(&self, path: String) -> FutureResult<Vec<DirEntry>>;

    /// Accumulate an edit against the session's base tree.
    fn write(&self, path: String, bytes: Vec<u8>) -> FutureResult<()>;

    /// The absolute host path of the lent workspace, when one was lent for
    /// this completion and resolved to an authorized mount.
    fn local_path(&self) -> Option<&std::path::Path> {
        None
    }
}

/// A function tool's model-visible output or failure text.
pub type ToolOutcome = Result<String, String>;

/// One bounded directory entry returned by `ToolHost::list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Entry name (never an OS path).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_directory: bool,
}
