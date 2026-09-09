//! A FIFO-scripted `WasiModelCtx` recording every request.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::FutureExt as _;
use omnia_wasi_model::{
    Answer, Error, Format, FutureResult, Limits, Request, Tool, ToolHost, WasiModelCtx,
};

use crate::{Exchange, Script, Seen, SeenFormat};

const CHECK_TOOL: &str = "check";

/// One step a scripted turn drives through the session before answering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// A declared function tool, answered by the guest's handler.
    Tool {
        /// The declared tool name.
        name: String,
        /// JSON arguments.
        arguments: String,
    },
    /// The host-injected `read` over the lent workspace.
    Read {
        /// Workspace-relative path.
        path: String,
    },
    /// The host-injected `write` over the lent workspace.
    Write {
        /// Workspace-relative path.
        path: String,
        /// The bytes written.
        bytes: Vec<u8>,
    },
    /// The host-injected `list` over the lent workspace.
    List {
        /// Workspace-relative directory path (empty for the root).
        path: String,
    },
}

/// One scripted completion: the steps the backend drives through the
/// session, then the answer.
#[derive(Clone, Debug)]
pub struct Completion {
    /// Tool calls and workspace operations driven, in order, before the
    /// answer returns.
    pub steps: Vec<Step>,
    /// The answer, offered to the guest's `check` first when the request
    /// asks for one.
    pub answer: Answer,
}

impl Completion {
    const fn answer(answer: Answer) -> Self {
        Self {
            steps: Vec::new(),
            answer,
        }
    }
}

/// A FIFO model backend recording every request and session exchange.
///
/// The host-side counterpart of the guest `Scripted` double: answers are
/// strings (or full [`Answer`]s) the `omnia:model` host hands to the guest
/// unvalidated, and a turn may drive declared tools and the host-injected
/// workspace tools through the session before answering. A request with
/// `check` set plays the backend's loop: each scripted turn is one
/// candidate offered to the guest's `check` and recorded; a rejection
/// advances to the next turn, and a script exhausted on a rejection is
/// `budget-exhausted` carrying the correction. A hard session failure —
/// undeclared tool, exhausted budget, closed results stream, a workspace op
/// without a grant — fails the completion the way a real backend's would. A
/// call past the script fails the completion the guest sees — a panic
/// inside a wasmtime host call would be worse — and records the overrun so
/// [`ScriptedModel::assert_exhausted`] still fails the test.
///
/// ```no_run
/// use omnia_test::host::{Backends, Deployment, ScriptedModel};
/// use omnia_wasi_model::WasiModel;
///
/// # async fn example(guest: &'static str) -> anyhow::Result<()> {
/// let model = ScriptedModel::answering(["42"]).calling(0, [("lookup", "{}")]);
/// let backends = Backends::defaults().await.model(model);
/// Deployment::new().guest("agent", guest).run_host::<WasiModel, _>(backends.clone()).await?;
/// assert_eq!(backends.model.exchanges()[0].tool, "lookup");
/// backends.model.assert_exhausted();
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct ScriptedModel {
    script: Script<Seen, Completion>,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
    lent: Arc<Mutex<Vec<Option<PathBuf>>>>,
    limits: Limits,
}

impl Default for ScriptedModel {
    fn default() -> Self {
        Self::replying([])
    }
}

impl ScriptedModel {
    /// A script of ordered answer strings.
    pub fn answering<S: Into<String>>(answers: impl IntoIterator<Item = S>) -> Self {
        Self::replying(answers.into_iter().map(|answer| Answer::from(answer.into())))
    }

    /// A script of ordered full answers — text, usage, and transcript.
    pub fn replying(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            script: Script::new(answers.into_iter().map(Completion::answer)),
            exchanges: Arc::new(Mutex::new(Vec::new())),
            lent: Arc::new(Mutex::new(Vec::new())),
            limits: Limits::default(),
        }
    }

    /// Attaches `(tool, arguments)` calls to the turn at `index`; the
    /// backend drives them through the session before that turn answers.
    ///
    /// # Panics
    ///
    /// Panics when no turn is scripted at `index`.
    #[must_use]
    #[track_caller]
    pub fn calling<T: Into<String>, A: Into<String>>(
        self, index: usize, calls: impl IntoIterator<Item = (T, A)>,
    ) -> Self {
        let steps: Vec<Step> = calls
            .into_iter()
            .map(|(name, arguments)| Step::Tool {
                name: name.into(),
                arguments: arguments.into(),
            })
            .collect();
        self.stepping(index, steps)
    }

    /// Attaches a workspace `read` of `path` to the turn at `index`.
    ///
    /// # Panics
    ///
    /// Panics when no turn is scripted at `index`.
    #[must_use]
    #[track_caller]
    pub fn reading(self, index: usize, path: impl Into<String>) -> Self {
        self.stepping(index, [Step::Read { path: path.into() }])
    }

    /// Attaches a workspace `write` of `bytes` to `path` to the turn at
    /// `index`.
    ///
    /// # Panics
    ///
    /// Panics when no turn is scripted at `index`.
    #[must_use]
    #[track_caller]
    pub fn writing(self, index: usize, path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.stepping(
            index,
            [Step::Write {
                path: path.into(),
                bytes: bytes.into(),
            }],
        )
    }

    /// Attaches a workspace `list` of `path` to the turn at `index`; the
    /// recorded outcome is the sorted entry names joined with `,`.
    ///
    /// # Panics
    ///
    /// Panics when no turn is scripted at `index`.
    #[must_use]
    #[track_caller]
    pub fn listing(self, index: usize, path: impl Into<String>) -> Self {
        self.stepping(index, [Step::List { path: path.into() }])
    }

    /// The session limits this backend reports to the host.
    #[must_use]
    pub const fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Answers every completion past the scripted turns with `answer`
    /// instead of failing.
    ///
    /// # Panics
    ///
    /// Panics if a fallback was already set.
    #[must_use]
    pub fn then(self, answer: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self {
            script: self.script.then(move || Completion::answer(Answer::from(answer()))),
            ..self
        }
    }

    /// Every request in call order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.script.seen()
    }

    /// Every driven exchange in call order: declared tools under their own
    /// names, workspace operations under the host-injected `read`, `write`,
    /// and `list`, and each `check` round under `check` (candidate in,
    /// correction as the `Err` outcome).
    ///
    /// # Panics
    ///
    /// Panics if the exchange lock is poisoned.
    #[must_use]
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().expect("exchanges lock").clone()
    }

    /// The host path of the workspace lent to each completion, in call order
    /// (`None` when nothing was lent or the lend was refused).
    ///
    /// # Panics
    ///
    /// Panics if the lend lock is poisoned.
    #[must_use]
    pub fn lent(&self) -> Vec<Option<PathBuf>> {
        self.lent.lock().expect("lent lock").clone()
    }

    /// Asserts that every scripted answer was consumed and no completion was
    /// requested past the script.
    ///
    /// # Panics
    ///
    /// Panics naming the number of unconsumed turns or overruns.
    #[track_caller]
    pub fn assert_exhausted(&self) {
        self.script.assert_exhausted();
    }

    #[track_caller]
    fn stepping(self, index: usize, steps: impl IntoIterator<Item = Step>) -> Self {
        let steps: Vec<Step> = steps.into_iter().collect();
        Self {
            script: self.script.edit(index, |turn| turn.steps.extend(steps)),
            ..self
        }
    }
}

impl ScriptedModel {
    // Pop the next scripted turn for `request`, or the backend failure a
    // guest completing past the script must see — not a host panic inside
    // the wasmtime call; `try_next` records the overrun for
    // `assert_exhausted`.
    fn turn(&self, request: &Request) -> anyhow::Result<Completion> {
        self.script.try_next(Seen::from(request)).ok_or_else(|| {
            let consumed = self.script.seen().len();
            anyhow::anyhow!(
                "model script exhausted: {} turn(s) consumed, none scripted for request \
                 #{consumed}",
                consumed - 1
            )
        })
    }

    async fn drive(&self, tool_host: &Arc<dyn ToolHost>, steps: Vec<Step>) -> anyhow::Result<()> {
        for step in steps {
            let exchange = match step {
                Step::Tool { name, arguments } => Exchange {
                    outcome: tool_host.call_tool(name.clone(), arguments.clone()).await?,
                    tool: name,
                    arguments,
                },
                Step::Read { path } => {
                    let bytes = tool_host.read(path.clone()).await?;
                    Exchange {
                        tool: "read".to_owned(),
                        arguments: path,
                        outcome: Ok(String::from_utf8_lossy(&bytes).into_owned()),
                    }
                }
                Step::Write { path, bytes } => {
                    tool_host.write(path.clone(), bytes).await?;
                    Exchange {
                        tool: "write".to_owned(),
                        arguments: path,
                        outcome: Ok(String::new()),
                    }
                }
                Step::List { path } => {
                    let mut names: Vec<String> = tool_host
                        .list(path.clone())
                        .await?
                        .into_iter()
                        .map(|entry| entry.name)
                        .collect();
                    names.sort();
                    Exchange {
                        tool: "list".to_owned(),
                        arguments: path,
                        outcome: Ok(names.join(",")),
                    }
                }
            };
            self.exchanges.lock().expect("exchanges lock").push(exchange);
        }
        Ok(())
    }
}

impl WasiModelCtx for ScriptedModel {
    fn complete(&self, request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer> {
        self.lent.lock().expect("lent lock").push(tool_host.local_path().map(Path::to_path_buf));
        let this = self.clone();
        async move {
            loop {
                let turn = this.turn(&request)?;
                this.drive(&tool_host, turn.steps).await?;
                if !request.check {
                    return Ok(turn.answer);
                }

                let outcome = tool_host.check(turn.answer.answer.clone()).await?;
                this.exchanges.lock().expect("exchanges lock").push(Exchange {
                    tool: CHECK_TOOL.to_owned(),
                    arguments: turn.answer.answer.clone(),
                    outcome: outcome.clone().map(|()| String::new()),
                });
                match outcome {
                    Ok(()) => return Ok(turn.answer),
                    // The next scripted answer is the model's corrected
                    // attempt; none left is the backend's round budget,
                    // typed so the host surfaces it as `budget-exhausted`.
                    Err(correction) if this.script.remaining() == 0 => {
                        return Err(Error::BudgetExhausted(correction).into());
                    }
                    Err(_) => {}
                }
            }
        }
        .boxed()
    }

    fn limits(&self) -> Limits {
        self.limits
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
            // The descriptor lend cannot cross into a plain record; the
            // subpath beneath the lent root is what the guest chose.
            workspace: request.grants.workspace.as_ref().map(|grant| grant.subpath.clone()),
            check: request.check,
        }
    }
}
