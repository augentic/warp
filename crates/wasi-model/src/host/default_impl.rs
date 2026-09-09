//! `ModelDefault` — the crate's default, deterministic (echo) backend.
//!
//! It connects with zero configuration and echoes the request's last message
//! back as the answer, shaped to the request's `format`, so guest wiring and
//! prompt assembly can be smoke-tested with no live model. `format::schema`
//! cannot be satisfied by an echo (no fabricated value can conform to an
//! arbitrary guest schema), so those completions fail loud. A request that
//! asks for a `check` gets exactly one: the echo has no second candidate, so
//! a rejection is `budget-exhausted` carrying the guest's correction.
//! Deployments bind a real backend (`omnia-genai`, `omnia-cursor`); tests
//! define inline canned `WasiModelCtx` impls that return a fixed answer.

use std::fmt::Debug;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::FutureExt as _;
use omnia_core::Backend;
use serde_json::json;

use crate::host::generated::omnia::model::completion::{Format, Request};
use crate::host::{Answer, Error, FutureResult, ToolHost, WasiModelCtx};

/// Echo default implementation of `wasi-model`: it starts without
/// configuration and answers every completion with its own prompt.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelDefault;

impl Backend for ModelDefault {
    type ConnectOptions = omnia_core::NoOptions;

    fn connect_with(
        _options: Self::ConnectOptions,
    ) -> impl std::future::Future<Output = Result<Self>> {
        std::future::ready(Ok(Self))
    }
}

impl WasiModelCtx for ModelDefault {
    fn complete(&self, request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer> {
        let answer = echo(&request);
        let check = request.check;
        async move {
            let answer = answer?;
            if check {
                tool_host
                    .check(answer.clone())
                    .await?
                    .map_err(|correction| anyhow!(Error::BudgetExhausted(correction)))?;
            }
            Ok(Answer::from(answer))
        }
        .boxed()
    }
}

// Echo the last message's content, shaped to the request's `format`.
fn echo(request: &Request) -> Result<String> {
    let prompt = request.messages.last().map(|message| message.content.clone()).unwrap_or_default();
    match &request.format {
        Format::Text => Ok(prompt),
        Format::Json => Ok(json!({ "echo": prompt }).to_string()),
        Format::Schema(_) => Err(anyhow!(
            "the default echo backend cannot satisfy format::schema: bind a real model backend"
        )),
    }
}
