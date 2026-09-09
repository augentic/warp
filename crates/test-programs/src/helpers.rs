//! Shared helpers for the guest scenario programs in `programs/<capability>/`.

use omnia_guest::model::{Function, Message, Role, Tool};
use omnia_wasi_model::completion;
use schemars::JsonSchema;
use serde::Deserialize;

/// The `verdict` JSON Schema several scenarios request.
pub const VERDICT_SCHEMA: &str =
    r#"{"type":"object","properties":{"verdict":{"type":"string"}},"required":["verdict"]}"#;

/// The typed answer the `check_*` scenarios ask for.
#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Verdict {
    /// `pass` or `fail`.
    pub verdict: String,
    /// What was wrong, when `fail`.
    pub findings: Vec<String>,
}

impl Verdict {
    /// The check every `check_*` scenario applies: a `pass` with no findings.
    ///
    /// # Errors
    ///
    /// Returns the findings a candidate must resolve.
    pub fn passing(&self) -> Result<(), Vec<String>> {
        let mut findings = Vec::new();
        if self.verdict != "pass" {
            findings.push("verdict must be `pass`".to_owned());
        }
        if !self.findings.is_empty() {
            findings.push("findings must be empty".to_owned());
        }
        if findings.is_empty() { Ok(()) } else { Err(findings) }
    }
}

/// One user chat turn.
#[must_use]
pub fn user(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_owned(),
    }
}

/// A declared `lookup` function tool.
#[must_use]
pub fn lookup() -> Tool {
    Tool::Function(
        Function::builder().name("lookup").description("test lookup").parameters("{}").build(),
    )
}

/// The raw-bindings `lookup` function tool.
#[must_use]
pub fn raw_lookup() -> completion::Function {
    completion::Function {
        name: "lookup".to_owned(),
        description: "test lookup".to_owned(),
        parameters: "{}".to_owned(),
    }
}

/// A minimal raw-bindings request — one `hi` user turn, `format::text` —
/// with the given function tools and grants.
#[must_use]
pub fn raw_request(
    tools: Vec<completion::Function>, grants: completion::Grants,
) -> completion::Request {
    completion::Request {
        model: None,
        system: None,
        messages: vec![completion::Message {
            role: completion::Role::User,
            content: "hi".to_owned(),
        }],
        generation: None,
        format: completion::Format::Text,
        tools: tools.into_iter().map(completion::Tool::Function).collect(),
        grants,
        check: false,
    }
}
