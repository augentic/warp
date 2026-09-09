//! The `create` host binding and its reply pipeline.

use std::fmt;
use std::sync::Arc;

use anyhow::anyhow;
use futures::{FutureExt as _, future};
use omnia_core::HasMounts;
use wasmtime::component::{Accessor, FutureReader, StreamReader};

use crate::host::generated::omnia::model::completion::{
    Host, HostWithStore, Reply, Session, ToolResult,
};
use crate::host::session::{CallsProducer, ReplyTask, ResultsConsumer, SessionClose, ToolSession};
use crate::host::tool_host::DirEntry;
use crate::host::workspace::{self, Workspace};
use crate::host::{
    Error, FutureResult, Request, ToolHost, ToolOutcome, WasiModel, WasiModelCtxView,
};

impl<T> HostWithStore<T> for WasiModel
where
    T: HasMounts,
{
    fn create(
        accessor: &Accessor<T, Self>, request: Request, mut results: StreamReader<ToolResult>,
    ) -> impl Future<Output = Result<Session, Error>> {
        std::future::ready(accessor.with(|mut access| {
            if let Err(error) = request.validate() {
                results.close(&mut access)?;
                return Err(error);
            }

            // get workspace
            let workspace = if let Some(grant) = request.grants.workspace.as_ref() {
                let mounts = access.data_mut().mounts();
                let descriptor = access.get().table.get(&grant.root)?;

                match workspace::resolve(descriptor, &mounts, grant) {
                    Ok(workspace) => Some(workspace),
                    Err(error) => {
                        results.close(&mut access)?;
                        return Err(error.into());
                    }
                }
            } else {
                None
            };

            // call model backend with request and tool host "closure"
            let limits = access.get().ctx.limits();
            let allowed = request.tool_names();

            let (session, calls_rx) = ToolSession::new(limits, allowed);
            let tool_host: Arc<dyn ToolHost> = Arc::new(BoundToolHost {
                session: Arc::clone(&session),
                workspace,
            });
            let answer = access.get().ctx.complete(request, tool_host);

            results.pipe(&mut access, ResultsConsumer::new(Arc::clone(&session)))?;
            let mut calls = StreamReader::new(&mut access, CallsProducer::new(calls_rx))?;

            // extract reply from answer
            let close = SessionClose::new(Arc::clone(&session));
            let reply_task = ReplyTask::spawn(async move {
                let _close = close;
                match answer.await {
                    Ok(answer) => session.take_error().map_or_else(
                        || {
                            Ok(Reply {
                                answer: answer.answer,
                                usage: answer.usage.map(Into::into),
                            })
                        },
                        Err,
                    ),
                    // A backend that fails with a typed `Error` (a rejected
                    // check exhausting its rounds) keeps it; anything else
                    // is a `backend` failure.
                    Err(error) => Err(session
                        .take_error()
                        .unwrap_or_else(|| error.downcast::<Error>().unwrap_or_else(Into::into))),
                }
            });

            // map the reply task to a future
            let reply_fut = reply_task.join().map(Ok::<_, wasmtime::Error>);
            let reply = match FutureReader::new(&mut access, reply_fut) {
                Ok(reply) => reply,
                Err(error) => {
                    calls.close(&mut access)?;
                    return Err(error.into());
                }
            };

            Ok(Session { calls, reply })
        }))
    }
}

impl Host for WasiModelCtxView<'_> {
    fn convert_error(&mut self, err: Error) -> wasmtime::Result<Error> {
        Ok(err)
    }
}

// The bound tool host, built fresh per completion from the request's grants
// and the session channels the `create` binding minted.
struct BoundToolHost {
    session: Arc<ToolSession>,
    workspace: Option<Workspace>,
}

// Manual because the session and workspace internals (channels, capability
// handles) carry no useful state to print; the lent path is the identity.
impl fmt::Debug for BoundToolHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundToolHost")
            .field("workspace", &self.workspace.as_ref().map(Workspace::local_path))
            .finish_non_exhaustive()
    }
}

impl BoundToolHost {
    // Run `op` against the lent workspace, or fail when none was granted.
    fn with_workspace<R: Send + 'static>(
        &self, op: &str, path: String, f: impl FnOnce(&Workspace, String) -> FutureResult<R>,
    ) -> FutureResult<R> {
        match &self.workspace {
            Some(workspace) => f(workspace, path),
            None => future::err(anyhow!("{op}(`{path}`) requires grants.workspace")).boxed(),
        }
    }
}

impl ToolHost for BoundToolHost {
    fn call_tool(&self, name: String, arguments: String) -> FutureResult<ToolOutcome> {
        Arc::clone(&self.session).call(name, arguments)
    }

    fn check(&self, candidate: String) -> FutureResult<Result<(), String>> {
        Arc::clone(&self.session).check(candidate)
    }

    fn read(&self, path: String) -> FutureResult<Vec<u8>> {
        self.with_workspace("read", path, Workspace::read)
    }

    fn list(&self, path: String) -> FutureResult<Vec<DirEntry>> {
        self.with_workspace("list", path, Workspace::list)
    }

    fn write(&self, path: String, bytes: Vec<u8>) -> FutureResult<()> {
        self.with_workspace("write", path, move |workspace, path| workspace.write(path, bytes))
    }

    fn local_path(&self) -> Option<&std::path::Path> {
        self.workspace.as_ref().map(Workspace::local_path)
    }
}
