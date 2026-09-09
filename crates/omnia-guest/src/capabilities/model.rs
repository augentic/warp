//! Prompt-completion (model) capability.
//!
//! Target-independent mirrors of the `omnia:model/completion` records. The
//! one record that cannot cross off `wasm32` is the `grants.workspace`
//! descriptor lend — a `wasi:filesystem` resource that only exists on
//! `wasm32` — so a guest names the directory with the plain
//! [`Request::workspace`] path and the `wasm32` default body resolves it
//! against the guest's preopens at the call site: the longest preopen whose
//! name prefixes the path becomes the lent root descriptor and the
//! remainder rides as the grant's subpath.
//!
//! Acceptance of an answer is the guest's: a request with [`Request::check`]
//! set receives each candidate as a tool call named `check` and answers
//! `Ok` to finish or `Err(correction)` to send the model round again. The
//! typed [`Question`] runs that exchange for a `JsonSchema + Deserialize`
//! answer type.

mod question;

use std::future::Future;

use serde::de::DeserializeOwned;

pub use self::question::{Findings, Question, ToolFuture, Tools};

/// The reserved tool name a `check` candidate arrives under.
pub const CHECK_TOOL: &str = "check";

/// Complete request for one completion.
#[derive(Clone, Debug, Default, PartialEq, Eq, bon::Builder)]
pub struct Request {
    /// Opaque model id hint; passed through unchanged. Backend may override.
    #[builder(into)]
    pub model: Option<String>,
    /// System / instructions channel.
    #[builder(into)]
    pub system: Option<String>,
    /// Chat turns sent to the provider. Must not be empty.
    pub messages: Vec<Message>,
    /// Sampling and length controls.
    pub generation: Option<Generation>,
    /// Output shape hint steering the provider; nothing is validated
    /// against it.
    #[builder(default)]
    pub format: Format,
    /// Guest-declared functions and MCP grants merged with host-injected
    /// tools at the backend. The names `read`, `list`, `write`, and `check`
    /// are reserved.
    #[builder(default)]
    pub tools: Vec<Tool>,
    /// Ask the guest to accept each candidate answer: the candidate arrives
    /// at the [`Model::complete_with`] handler as a [`ToolCall`] named
    /// `check` whose `arguments` are the candidate text. `Ok` accepts it as
    /// the reply; `Err(text)` is appended to the conversation verbatim as
    /// the correction turn and the backend goes round again.
    #[builder(default)]
    pub check: bool,
    /// Deployment-local path of the directory to lend through
    /// `grants.workspace`, giving the backend (and any spawned agent) that
    /// directory. On `wasm32` the path must sit on (or beneath) a preopen —
    /// `"."` lends the shared project mount, `"/mount/sub"` lends a
    /// subdirectory of `/mount`. Off `wasm32` it is a host path the
    /// provider consumes directly. `None` lends nothing.
    #[builder(into)]
    pub workspace: Option<String>,
}

/// One chat turn passed to the provider API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Turn author.
    pub role: Role,
    /// Turn body text.
    pub content: String,
}

/// Chat turn author.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// System / instructions channel.
    System,
    /// End-user turn.
    User,
    /// Model turn.
    Assistant,
}

/// Sampling and length controls. Omitted fields defer to backend defaults.
#[derive(Clone, Debug, Default, PartialEq, bon::Builder)]
pub struct Generation {
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f32>,
    /// Maximum output tokens.
    pub max_tokens: Option<u32>,
    /// Sequences that halt generation.
    #[builder(default)]
    pub stop: Vec<String>,
    /// Seed for reproducible sampling when the provider supports it.
    pub seed: Option<u64>,
    /// Reasoning-effort hint for thinking-capable models.
    pub effort: Option<Effort>,
}

// The float fields are sampling controls set from configuration values; NaN
// is never a meaningful setting, so total equality holds.
impl Eq for Generation {}

/// Reasoning-effort hint for models that expose a thinking budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    /// Least reasoning; lowest latency and cost.
    Minimal,
    /// Reduced reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Most reasoning; highest latency and cost.
    High,
}

/// Output shape hint steering the provider.
///
/// Passed as `response_format` where the provider constrains decoding,
/// instruction prose where it does not. Nothing is validated against it;
/// acceptance is the request's `check`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// Plain text.
    #[default]
    Text,
    /// Steer toward a JSON object.
    Json,
    /// Steer toward the given JSON Schema.
    Schema(SchemaFormat),
}

/// JSON Schema steering output.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct SchemaFormat {
    /// Schema name passed to the provider (e.g. `review_result`).
    #[builder(into)]
    pub name: String,
    /// JSON Schema document the answer should conform to.
    #[builder(into)]
    pub schema: String,
}

/// A tool offered to the model: a guest-declared function or an MCP server
/// grant carrying its own endpoint URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Guest-declared function tool.
    Function(Function),
    /// MCP server grant.
    Mcp(McpGrant),
}

/// Guest-declared function tool advertised to the model.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct Function {
    /// Tool name. Must not collide with the reserved names (`read`, `list`,
    /// `write`, `check`).
    #[builder(into)]
    pub name: String,
    /// Natural-language description for the model.
    #[builder(into)]
    pub description: String,
    /// JSON Schema for the tool's arguments object.
    #[builder(into)]
    pub parameters: String,
}

/// Remote MCP server offered to the model for this completion.
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
pub struct McpGrant {
    /// Logical server name identifying the server (e.g. in `.cursor/mcp.json`).
    #[builder(into)]
    pub name: String,
    /// Tool allowlist; empty exposes every tool the server advertises.
    #[builder(default)]
    pub tools: Vec<String>,
    /// MCP server endpoint URL.
    #[builder(into)]
    pub url: String,
}

/// One completion result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The text the request's `check` accepted — or, without a check, the
    /// model's final text as the backend received it.
    pub answer: String,
    /// Token accounting, when the backend reports it.
    pub usage: Option<Usage>,
}

/// Token accounting for one completion, when the backend reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Prompt tokens consumed.
    pub input_tokens: u32,
    /// Completion tokens produced.
    pub output_tokens: u32,
    /// Reasoning tokens, for models that bill them separately.
    pub reasoning_tokens: Option<u32>,
}

/// Typed completion failure, mirroring the `omnia:model` error variant.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The request itself is malformed (empty `messages`, reserved tool
    /// name, schema that is not JSON); retrying without changing it is
    /// pointless.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Iteration, token, or time budget exhausted. When the last round ended
    /// on a rejected `check`, the detail is that correction text.
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    /// Non-repairable tool error.
    #[error("tool failed: {0}")]
    ToolFailed(String),
    /// Transport, process, or provider failure.
    #[error("backend failure: {0}")]
    Backend(String),
}

/// One tool invocation the model asked the guest to run, delivered to the
/// [`Model::complete_with`] handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    /// Correlation id the session answers by; the handler never needs it.
    pub id: String,
    /// The declared function-tool name the model called, or the reserved
    /// `check` when the request asked for one.
    pub name: String,
    /// JSON arguments object for the tool, per its declared parameters
    /// schema; the candidate text for `check`.
    pub arguments: String,
}

impl ToolCall {
    /// Deserialize the call's `arguments` into `T`.
    ///
    /// # Errors
    ///
    /// Returns model-visible failure text when the arguments do not match `T`.
    pub fn arguments<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_str(&self.arguments).map_err(|error| format!("invalid arguments: {error}"))
    }
}

/// Prompt completion (Omnia Model).
///
/// Default WASM implementations delegate to `omnia:model/completion` via
/// `omnia-wasi-model`, opening the completion session and answering the
/// model's tool calls with the supplied closure; off `wasm32` the signatures
/// are bare so hosts and tests supply their own provider.
pub trait Model: Send + Sync {
    /// Single-shot completion returning one reply. Any tool call the model
    /// issues fails back to it; declare tools (or set `check`) and answer
    /// them through [`Model::complete_with`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    fn complete(&self, request: Request) -> impl Future<Output = Result<Reply, Error>> + Send;

    /// Single-shot completion returning one reply. Any tool call the model
    /// issues fails back to it; declare tools (or set `check`) and answer
    /// them through [`Model::complete_with`] instead.
    #[cfg(target_arch = "wasm32")]
    fn complete(&self, request: Request) -> impl Future<Output = Result<Reply, Error>> + Send {
        self.complete_with(request, |call: ToolCall| async move {
            Err::<String, String>(format!(
                "tool `{}` has no handler: answer tool calls with complete_with",
                call.name
            ))
        })
    }

    /// Completion with a tool closure: the model's tool calls arrive as
    /// [`ToolCall`] values answered serially by `handler` (over the caller's
    /// own locals and authority), and each result feeds the same model turn.
    /// Results correlate by id, so parallel handling stays available through
    /// the raw `omnia_wasi_model::completion` session bindings.
    #[cfg(not(target_arch = "wasm32"))]
    fn complete_with<H, F>(
        &self, request: Request, handler: H,
    ) -> impl Future<Output = Result<Reply, Error>> + Send
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send;

    /// Completion with a tool closure: the model's tool calls arrive as
    /// [`ToolCall`] values answered serially by `handler` (over the caller's
    /// own locals and authority), and each result feeds the same model turn.
    /// Results correlate by id, so parallel handling stays available through
    /// the raw `omnia_wasi_model::completion` session bindings.
    #[cfg(target_arch = "wasm32")]
    fn complete_with<H, F>(
        &self, request: Request, mut handler: H,
    ) -> impl Future<Output = Result<Reply, Error>> + Send
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send,
    {
        use omnia_wasi_model::{completion, wit_stream};
        use wasip3::filesystem::preopens;

        async move {
            // The lent workspace borrows one of these descriptors, so the
            // table must outlive the `create` call below.
            let directories =
                if request.workspace.is_some() { preopens::get_directories() } else { vec![] };
            let workspace = match request.workspace.as_deref() {
                None => None,
                Some(path) => match resolve_lend(&directories, path) {
                    Some((root, subpath)) => Some(completion::WorkspaceGrant {
                        root,
                        subpath: subpath.to_string(),
                    }),
                    None => {
                        return Err(Error::InvalidRequest(format!(
                            "workspace lend `{path}` matches no preopen"
                        )));
                    }
                },
            };

            let wire = completion::Request {
                model: request.model,
                system: request.system,
                messages: request.messages.into_iter().map(Into::into).collect(),
                generation: request.generation.map(Into::into),
                format: request.format.into(),
                tools: request.tools.into_iter().map(Into::into).collect(),
                grants: completion::Grants { workspace },
                check: request.check,
            };

            let (mut results, results_rx) = wit_stream::new();
            let session = completion::create(wire, results_rx).await.map_err(Error::from)?;
            let completion::Session { mut calls, reply } = session;

            // Serial by design: each result feeds the same model turn. A
            // rejected write means the host stopped reading results; the
            // loop then ends on the closed calls stream.
            let calls_loop = async {
                while let Some(call) = calls.next().await {
                    let id = call.id.clone();
                    let output = handler(ToolCall {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    })
                    .await;
                    let _ = results.write_one(completion::ToolResult { id, output }).await;
                }
            };

            // The host always resolves the reply future, so joining cannot
            // hang on a well-behaved host; either side closing its stream
            // ends the other's loop.
            let ((), outcome) =
                futures::join!(calls_loop, std::future::IntoFuture::into_future(reply));
            outcome.map(Into::into).map_err(Into::into)
        }
    }
}

delegate_deref!(Model {
    fn complete(&self, request: Request) -> impl Future<Output = Result<Reply, Error>> + Send {
        (**self).complete(request)
    }

    fn complete_with<H, F>(
        &self, request: Request, handler: H,
    ) -> impl Future<Output = Result<Reply, Error>> + Send
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send,
    {
        (**self).complete_with(request, handler)
    }
});

/// The WASI-backed provider a `wasm32` guest hands its wasm-free core; the
/// default method body carries the whole delegation.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug)]
pub struct WasiModel;

#[cfg(target_arch = "wasm32")]
impl Model for WasiModel {}

/// Resolve a lend path against the guest's preopens: the longest preopen
/// name that equals the path or prefixes it at a `/` boundary wins; the
/// remainder becomes the grant's subpath (empty for the mount itself).
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_lend<'a, D>(directories: &'a [(D, String)], path: &'a str) -> Option<(&'a D, &'a str)> {
    directories
        .iter()
        .filter_map(|(dir, name)| Some((dir, lend_subpath(name, path)?)))
        .max_by_key(|(_, subpath)| std::cmp::Reverse(subpath.len()))
}

/// The subpath of `path` beneath the preopen `name`, when `name` covers it.
#[cfg(any(target_arch = "wasm32", test))]
fn lend_subpath<'a>(name: &str, path: &'a str) -> Option<&'a str> {
    if path == name {
        return Some("");
    }
    path.strip_prefix(name)?.strip_prefix('/').filter(|rest| !rest.is_empty())
}

/// Mirror-to-wire conversions between the target-independent records above
/// and the `omnia:model/completion` bindings.
#[cfg(target_arch = "wasm32")]
mod wire {
    use omnia_wasi_model::completion;

    use super::{
        Effort, Error, Format, Function, Generation, McpGrant, Message, Reply, Role, Tool, Usage,
    };

    impl From<Message> for completion::Message {
        fn from(message: Message) -> Self {
            Self {
                role: message.role.into(),
                content: message.content,
            }
        }
    }

    impl From<Role> for completion::Role {
        fn from(role: Role) -> Self {
            match role {
                Role::System => Self::System,
                Role::User => Self::User,
                Role::Assistant => Self::Assistant,
            }
        }
    }

    impl From<Generation> for completion::Generation {
        fn from(generation: Generation) -> Self {
            Self {
                temperature: generation.temperature,
                top_p: generation.top_p,
                max_tokens: generation.max_tokens,
                stop: generation.stop,
                seed: generation.seed,
                effort: generation.effort.map(Into::into),
            }
        }
    }

    impl From<Effort> for completion::Effort {
        fn from(effort: Effort) -> Self {
            match effort {
                Effort::Minimal => Self::Minimal,
                Effort::Low => Self::Low,
                Effort::Medium => Self::Medium,
                Effort::High => Self::High,
            }
        }
    }

    impl From<Format> for completion::Format {
        fn from(format: Format) -> Self {
            match format {
                Format::Text => Self::Text,
                Format::Json => Self::Json,
                Format::Schema(s) => Self::Schema(completion::Schema {
                    name: s.name,
                    schema: s.schema,
                }),
            }
        }
    }

    impl From<Tool> for completion::Tool {
        fn from(tool: Tool) -> Self {
            match tool {
                Tool::Function(Function {
                    name,
                    description,
                    parameters,
                }) => Self::Function(completion::Function {
                    name,
                    description,
                    parameters,
                }),
                Tool::Mcp(McpGrant { name, tools, url }) => {
                    Self::Mcp(completion::Mcp { name, tools, url })
                }
            }
        }
    }

    impl From<completion::Reply> for Reply {
        fn from(reply: completion::Reply) -> Self {
            Self {
                answer: reply.answer,
                usage: reply.usage.map(Into::into),
            }
        }
    }

    impl From<completion::Usage> for Usage {
        fn from(usage: completion::Usage) -> Self {
            Self {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                reasoning_tokens: usage.reasoning_tokens,
            }
        }
    }

    impl From<completion::Error> for Error {
        fn from(error: completion::Error) -> Self {
            match error {
                completion::Error::InvalidRequest(detail) => Self::InvalidRequest(detail),
                completion::Error::BudgetExhausted(detail) => Self::BudgetExhausted(detail),
                completion::Error::ToolFailed(detail) => Self::ToolFailed(detail),
                completion::Error::Backend(detail) => Self::Backend(detail),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_lend;

    fn preopens() -> Vec<(u8, String)> {
        vec![(0, ".".to_string()), (1, "/emery-workspaces".to_string())]
    }

    #[test]
    fn resolves_mount() {
        let dirs = preopens();
        assert_eq!(resolve_lend(&dirs, ".").map(|(_, sub)| sub), Some(""));
        assert_eq!(resolve_lend(&dirs, "/emery-workspaces/ws-1").map(|(_, sub)| sub), Some("ws-1"));
        assert_eq!(
            resolve_lend(&dirs, "/emery-workspaces/ws-1/nested").map(|(_, sub)| sub),
            Some("ws-1/nested")
        );
    }

    #[test]
    fn refuses_paths() {
        let dirs = preopens();
        assert!(resolve_lend(&dirs, "/elsewhere").is_none());
        assert!(resolve_lend(&dirs, "/emery-workspaces-evil/x").is_none());
        assert!(resolve_lend(&dirs, "/emery-workspaces/").is_none());
    }
}
