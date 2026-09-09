//! The scripted pair: a FIFO `Model` and a keyed `Plugins` loader.

use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use omnia_guest::model::{
    CHECK_TOOL, Error, Format, Function, Model, Reply, Request, Tool, ToolCall,
};
use omnia_guest::plugins::{self, Digest, Plugin, PluginRef, Plugins};

use crate::{Exchange, Script, Seen, SeenFormat};

/// One scripted completion turn: the tool calls fed to the handler, then
/// the turn's result.
#[derive(Clone, Debug)]
pub struct Turn {
    /// Tool calls `complete_with` drives through the handler before the
    /// result returns.
    pub calls: Vec<ToolCall>,
    /// The turn's outcome.
    pub result: Result<Reply, Error>,
}

impl Turn {
    const fn result(result: Result<Reply, Error>) -> Self {
        Self {
            calls: Vec::new(),
            result,
        }
    }
}

/// A FIFO model script recording every request and handler exchange.
///
/// Each turn is a success or a typed failure and may carry scripted tool
/// calls: `complete_with` feeds them to the handler and records each
/// exchange before the turn's result returns. A call past the script panics;
/// [`Scripted::then`] opts into a fallback instead.
///
/// A request with `check` set plays the backend's loop: each scripted turn
/// is one candidate, offered to the handler as the reserved `check` call and
/// recorded. `Ok` returns that turn; `Err(correction)` advances to the next
/// turn, and a script exhausted on a rejection is `BudgetExhausted`
/// carrying the correction — one scripted answer per attempt.
///
/// ```
/// use omnia_guest::model::{Message, Model as _, Request, Role};
/// use omnia_test::guest::Scripted;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let model = Scripted::answering(["four"]);
/// let request = Request::builder()
///     .messages(vec![Message {
///         role: Role::User,
///         content: "2 + 2?".into(),
///     }])
///     .build();
/// assert_eq!(model.complete(request).await.unwrap().answer, "four");
/// assert_eq!(model.seen()[0].messages, ["2 + 2?"]);
/// model.assert_exhausted();
/// # });
/// ```
#[derive(Clone, Debug)]
pub struct Scripted {
    script: Script<Request, Turn>,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

impl Default for Scripted {
    fn default() -> Self {
        Self::new([])
    }
}

impl Scripted {
    /// A script of ordered completion results.
    pub fn new(results: impl IntoIterator<Item = Result<Reply, Error>>) -> Self {
        Self {
            script: Script::new(results.into_iter().map(Turn::result)),
            exchanges: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A success script of ordered answer strings.
    pub fn answering<S: Into<String>>(answers: impl IntoIterator<Item = S>) -> Self {
        Self::new(answers.into_iter().map(|answer| {
            Ok(Reply {
                answer: answer.into(),
                usage: None,
            })
        }))
    }

    /// Attaches tool calls to the scripted turn at `index`; the handler
    /// answers them before that turn's result returns.
    ///
    /// # Panics
    ///
    /// Panics when no turn is scripted at `index`.
    #[must_use]
    #[track_caller]
    pub fn calling(self, index: usize, calls: impl IntoIterator<Item = ToolCall>) -> Self {
        let calls: Vec<ToolCall> = calls.into_iter().collect();
        Self {
            script: self.script.edit(index, |turn| turn.calls.extend(calls)),
            exchanges: self.exchanges,
        }
    }

    /// Answers every completion past the scripted turns with `result`
    /// instead of panicking.
    ///
    /// # Panics
    ///
    /// Panics if a fallback was already set.
    #[must_use]
    pub fn then(self, result: impl Fn() -> Result<Reply, Error> + Send + Sync + 'static) -> Self {
        Self {
            script: self.script.then(move || Turn::result(result())),
            exchanges: self.exchanges,
        }
    }

    /// Every request in call order, as the handler built it.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.script.seen()
    }

    /// Every request in call order, projected to the rung-neutral record.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.script.seen().iter().map(Seen::from).collect()
    }

    /// Every handler exchange in call order.
    ///
    /// # Panics
    ///
    /// Panics if the exchange lock is poisoned.
    #[must_use]
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().expect("exchanges lock").clone()
    }

    /// Asserts that every scripted turn was consumed.
    ///
    /// # Panics
    ///
    /// Panics naming the number of unconsumed turns.
    #[track_caller]
    pub fn assert_exhausted(&self) {
        self.script.assert_exhausted();
    }
}

impl Model for Scripted {
    /// # Panics
    ///
    /// Panics if the turn has scripted tool calls or the request asks for a
    /// `check` (both require `complete_with`), or if the script is exhausted
    /// without a fallback.
    fn complete(&self, request: Request) -> impl Future<Output = Result<Reply, Error>> + Send {
        // A single-shot completion has no handler; scripting tool calls on
        // its turn, or asking it to check, is a harness bug.
        assert!(!request.check, "a check requires complete_with");
        let turn = self.script.next(request);
        assert!(turn.calls.is_empty(), "scripted tool calls require complete_with");
        ready(turn.result)
    }

    /// # Panics
    ///
    /// Panics if the script is exhausted without a fallback.
    fn complete_with<H, F>(
        &self, request: Request, mut handler: H,
    ) -> impl Future<Output = Result<Reply, Error>> + Send
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send,
    {
        let script = self.script.clone();
        let exchanges = Arc::clone(&self.exchanges);
        async move {
            let mut attempt = 0;
            loop {
                let turn = script.next(request.clone());
                for call in turn.calls {
                    let outcome = handler(call.clone()).await;
                    exchanges.lock().expect("exchanges lock").push(Exchange {
                        tool: call.name,
                        arguments: call.arguments,
                        outcome,
                    });
                }
                let (candidate, reply) = match (&request.check, turn.result) {
                    (false, result) | (true, result @ Err(_)) => return result,
                    (true, Ok(reply)) => (reply.answer.clone(), reply),
                };

                attempt += 1;
                let outcome = handler(ToolCall {
                    id: format!("check-{attempt}"),
                    name: CHECK_TOOL.to_owned(),
                    arguments: candidate.clone(),
                })
                .await;
                exchanges.lock().expect("exchanges lock").push(Exchange {
                    tool: CHECK_TOOL.to_owned(),
                    arguments: candidate,
                    outcome: outcome.clone(),
                });
                match outcome {
                    Ok(_) => return Ok(reply),
                    // The next scripted answer is the model's corrected
                    // attempt; none left is the backend's round budget.
                    Err(correction) if script.remaining() == 0 => {
                        return Err(Error::BudgetExhausted(correction));
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

impl From<&Request> for Seen {
    fn from(request: &Request) -> Self {
        Self {
            system: request.system.clone(),
            messages: request.messages.iter().map(|message| message.content.clone()).collect(),
            format: match &request.format {
                Format::Text => SeenFormat::Text,
                Format::Json => SeenFormat::Json,
                Format::Schema(schema) => SeenFormat::Schema {
                    name: schema.name.clone(),
                    schema: schema.schema.clone(),
                },
            },
            tools: request
                .tools
                .iter()
                .map(|tool| match tool {
                    Tool::Function(function) => function.name.clone(),
                    Tool::Mcp(mcp) => mcp.name.clone(),
                })
                .collect(),
            temperature: request.generation.as_ref().and_then(|generation| generation.temperature),
            workspace: request.workspace.clone(),
            check: request.check,
        }
    }
}

/// The function tools a request declares, in declaration order.
#[must_use]
pub fn function_tools(request: &Request) -> Vec<&Function> {
    request
        .tools
        .iter()
        .filter_map(|tool| match tool {
            Tool::Function(function) => Some(function),
            Tool::Mcp(_) => None,
        })
        .collect()
}

/// A keyed `Plugins` loader: per-package digests and refusals, every load
/// recorded.
///
/// Keyed rather than FIFO because the loader contract is per package: a
/// request for a package with a scripted refusal fails with it; otherwise
/// the load resolves, in order, to the digest scripted for the package, the
/// request's own pin, the loader-wide default set by [`defaulting`], or a
/// deterministic per-package placeholder. A pin that disagrees with the
/// resolved digest is refused before anything else, as the host would.
///
/// [`defaulting`]: Self::defaulting
///
/// ```
/// use omnia_guest::plugins::{Error, Location, PluginRef, Plugins as _};
/// use omnia_test::guest::ScriptedLoader;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let loader = ScriptedLoader::default().refuse("acme:bad", Error::Refused("banned".into()));
/// let bad = PluginRef::builder().package("acme:bad").location(Location::Registry(None)).build();
/// assert!(matches!(loader.load(&bad).await, Err(Error::Refused(_))));
/// let ok = PluginRef::builder().package("acme:ok").location(Location::Registry(None)).build();
/// assert_eq!(loader.load(&ok).await.unwrap().id(), "acme:ok");
/// assert_eq!(loader.loads().len(), 2);
/// # });
/// ```
#[derive(Clone, Debug, Default)]
pub struct ScriptedLoader {
    inner: Arc<LoaderInner>,
}

#[derive(Debug, Default)]
struct LoaderInner {
    digests: Mutex<BTreeMap<String, Digest>>,
    default: Mutex<Option<Digest>>,
    refusals: Mutex<BTreeMap<String, plugins::Error>>,
    loads: Mutex<Vec<PluginRef>>,
}

impl ScriptedLoader {
    /// Resolves `package` to `digest`.
    ///
    /// # Panics
    ///
    /// Panics if a lock is poisoned.
    #[must_use]
    pub fn digest(self, package: impl Into<String>, digest: Digest) -> Self {
        self.inner.digests.lock().expect("digests lock").insert(package.into(), digest);
        self
    }

    /// Resolves every package without a scripted digest or a pin of its own
    /// to `digest`, in place of the per-package placeholder — for suites that
    /// assert one fixed digest in their envelopes.
    ///
    /// # Panics
    ///
    /// Panics if a lock is poisoned.
    #[must_use]
    pub fn defaulting(self, digest: Digest) -> Self {
        *self.inner.default.lock().expect("default lock") = Some(digest);
        self
    }

    /// Fails every load of `package` with `error`.
    ///
    /// # Panics
    ///
    /// Panics if a lock is poisoned.
    #[must_use]
    pub fn refuse(self, package: impl Into<String>, error: plugins::Error) -> Self {
        self.inner.refusals.lock().expect("refusals lock").insert(package.into(), error);
        self
    }

    /// Every load request in call order.
    ///
    /// # Panics
    ///
    /// Panics if a lock is poisoned.
    #[must_use]
    pub fn loads(&self) -> Vec<PluginRef> {
        self.inner.loads.lock().expect("loads lock").clone()
    }

    fn resolve(&self, plugin: &PluginRef) -> Result<Plugin, plugins::Error> {
        self.inner.loads.lock().expect("loads lock").push(plugin.clone());
        if let Some(refusal) =
            self.inner.refusals.lock().expect("refusals lock").get(&plugin.package)
        {
            return Err(refusal.clone());
        }
        let resolved = self
            .inner
            .digests
            .lock()
            .expect("digests lock")
            .get(&plugin.package)
            .cloned()
            .or_else(|| plugin.digest.clone())
            .or_else(|| self.inner.default.lock().expect("default lock").clone())
            .unwrap_or_else(|| placeholder_digest(&plugin.package));
        match &plugin.digest {
            Some(pin) if *pin != resolved => Err(plugins::Error::Refused(format!(
                "package `{}` resolved to {resolved}, which is not the pinned {pin}",
                plugin.package
            ))),
            _ => Ok(Plugin::new(plugin.package.clone(), resolved)),
        }
    }
}

impl Plugins for ScriptedLoader {
    fn load(
        &self, plugin: &PluginRef,
    ) -> impl Future<Output = Result<Plugin, plugins::Error>> + Send {
        ready(self.resolve(plugin))
    }
}

// A well-formed, package-specific pin for loads no scenario scripted, so two
// such packages never look like the same content.
fn placeholder_digest(package: &str) -> Digest {
    let mut hasher = DefaultHasher::new();
    package.hash(&mut hasher);
    let word = format!("{:016x}", hasher.finish());
    format!("sha256:{}", word.repeat(4)).parse().expect("64 hex characters form a digest")
}
