//! `Question<T>`: a completion whose answer is one `T`.
//!
//! The request's `format` is `T`'s JSON Schema — a steering hint for the
//! provider — and acceptance is the request's `check`, answered here: each
//! candidate is deserialized into `T` and handed to the caller's closure;
//! a miss becomes the correction turn (one default template, replaceable
//! per question), a hit is captured and returned. The reply text is never
//! re-parsed.

use std::fmt;
use std::future::{Future, ready};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    CHECK_TOOL, Error, Format, Function, Message, Model, Request, Role, SchemaFormat, Tool,
    ToolCall,
};

/// What a check found wrong with a candidate, one finding per line of the
/// correction turn.
pub type Findings = Vec<String>;

/// The future a tool handler returns.
pub type ToolFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'static>>;

/// The caller's handler for the request's declared function tools;
/// [`Question::ask`] answers the reserved `check` in front of it.
pub type Tools = Box<dyn FnMut(ToolCall) -> ToolFuture + Send>;

/// Renders the correction turn for a rejected candidate from the previous
/// answer and the findings against it.
pub type Correction = Arc<dyn Fn(&str, &[String]) -> String + Send + Sync>;

/// A question whose answer is one `T`.
pub struct Question<T> {
    request: Request,
    correction: Correction,
    answer: PhantomData<fn() -> T>,
}

impl<T> Clone for Question<T> {
    fn clone(&self) -> Self {
        Self {
            request: self.request.clone(),
            correction: Arc::clone(&self.correction),
            answer: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Question<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Question").field("request", &self.request).finish_non_exhaustive()
    }
}

impl<T: JsonSchema + DeserializeOwned + Send> Question<T> {
    /// A question steered by `T`'s schema under `name`, with `check` set.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            request: Request {
                format: Format::Schema(SchemaFormat::of::<T>(name)),
                check: true,
                ..Request::default()
            },
            correction: Arc::new(default_correction),
            answer: PhantomData,
        }
    }

    /// The system / instructions channel.
    #[must_use]
    pub fn system(mut self, prose: impl Into<String>) -> Self {
        self.request.system = Some(prose.into());
        self
    }

    /// Opaque model id hint.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.request.model = Some(model.into());
        self
    }

    /// Chat turns preceding the one [`Self::ask`] adds.
    #[must_use]
    pub fn messages(mut self, turns: Vec<Message>) -> Self {
        self.request.messages = turns;
        self
    }

    /// Function tools and MCP grants offered to the model.
    #[must_use]
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.request.tools = tools;
        self
    }

    /// The directory lent through `grants.workspace`; see [`Request::workspace`].
    #[must_use]
    pub fn workspace(mut self, lend: impl Into<String>) -> Self {
        self.request.workspace = Some(lend.into());
        self
    }

    /// Post-process the steering schema — subject enums, exact counts, id
    /// patterns. Hints for the provider, never the gate.
    #[must_use]
    pub fn schema(mut self, edit: impl FnOnce(&mut Value)) -> Self {
        if let Format::Schema(spec) = &mut self.request.format
            && let Ok(mut schema) = serde_json::from_str::<Value>(&spec.schema)
        {
            edit(&mut schema);
            spec.schema = schema.to_string();
        }
        self
    }

    /// Replace the correction turn's template: `render(previous, findings)`
    /// is the user message a rejected candidate earns, `previous` being the
    /// candidate text and `findings` what the check held against it (or the
    /// one deserialization failure). The default is a `## Previous answer
    /// (rejected)` / `## Findings` template.
    #[must_use]
    pub fn correction(
        mut self, render: impl Fn(&str, &[String]) -> String + Send + Sync + 'static,
    ) -> Self {
        self.correction = Arc::new(render);
        self
    }

    /// The request as built so far.
    #[must_use]
    pub const fn request(&self) -> &Request {
        &self.request
    }

    /// One completion, `turn` appended as the user message. Each candidate
    /// the backend proposes is deserialized into `T` and judged by `check`:
    /// a deserialization failure or `Err(findings)` is the correction turn,
    /// `Ok` captures the `T` this returns.
    ///
    /// # Errors
    ///
    /// - `InvalidRequest` when the completion ended with no accepted answer
    ///   after a candidate failed to deserialize: the schema and `T`
    ///   disagree, which more rounds will not fix.
    /// - `BudgetExhausted(correction)` when the backend ran out of rounds on
    ///   a rejected candidate.
    /// - `Backend` when the backend finished with no accepted answer: it
    ///   never ran the check, or returned after a rejection.
    /// - Any other host error, unchanged.
    pub async fn ask<M: Model>(
        &self, model: &M, turn: impl Into<String>, tools: Option<Tools>,
        mut check: impl FnMut(&T) -> Result<(), Findings> + Send,
    ) -> Result<T, Error> {
        let mut request = self.request.clone();
        request.messages.push(Message {
            role: Role::User,
            content: turn.into(),
        });

        let mut accepted: Option<T> = None;
        let mut mismatch: Option<String> = None;
        let mut candidates = 0_usize;
        let mut tools = tools;
        let correction = &self.correction;
        let handler = |call: ToolCall| -> ToolFuture {
            if call.name != CHECK_TOOL {
                return match tools.as_mut() {
                    Some(tools) => tools(call),
                    None => Box::pin(ready(Err(format!(
                        "tool `{}` has no handler: pass one to Question::ask",
                        call.name
                    )))),
                };
            }
            candidates += 1;
            let result = match serde_json::from_str::<T>(&call.arguments) {
                Err(error) => {
                    let finding = format!("the answer does not match the expected shape: {error}");
                    mismatch = Some(finding.clone());
                    Err(correction(&call.arguments, &[finding]))
                }
                Ok(candidate) => match check(&candidate) {
                    Ok(()) => {
                        accepted = Some(candidate);
                        Ok(String::new())
                    }
                    Err(findings) => Err(correction(&call.arguments, &findings)),
                },
            };
            Box::pin(ready(result))
        };

        let outcome = model.complete_with(request, handler).await;
        match (outcome, accepted, mismatch) {
            (Ok(_), Some(answer), _) => Ok(answer),
            (Ok(_), None, _) if candidates == 0 => {
                Err(Error::Backend("the backend finished without running the check".to_owned()))
            }
            (Ok(_), None, _) => Err(Error::Backend(
                "the backend finished on a candidate the check rejected".to_owned(),
            )),
            // A candidate serde rejected means the schema and `T` disagree:
            // a guest bug more rounds would not have fixed.
            (Err(Error::BudgetExhausted(_)), None, Some(mismatch)) => {
                Err(Error::InvalidRequest(format!("schema and answer type disagree: {mismatch}")))
            }
            (Err(error), _, _) => Err(error),
        }
    }
}

// The default correction turn for a rejected candidate.
fn default_correction(previous: &str, findings: &[String]) -> String {
    format!(
        "## Previous answer (rejected)\n\n{previous}\n\n## Findings\n\n{}\n\nProduce a corrected, \
         complete answer that resolves every finding.",
        findings.join("\n")
    )
}

impl SchemaFormat {
    /// `T`'s JSON Schema (draft 2020-12) under `name`.
    #[must_use]
    pub fn of<T: JsonSchema>(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            schema: schema_of::<T>(),
        }
    }
}

impl Function {
    /// A function tool whose parameters are `T`'s JSON Schema.
    #[must_use]
    pub fn of<T: JsonSchema>(name: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: schema_of::<T>(),
        }
    }
}

fn schema_of<T: JsonSchema>() -> String {
    schemars::schema_for!(T).to_value().to_string()
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::{Question, default_correction};
    use crate::model::{Format, Function, SchemaFormat, ToolCall};

    #[derive(Debug, Deserialize, JsonSchema)]
    struct Verdict {
        verdict: String,
    }

    #[test]
    fn schema_of_type() {
        let format = SchemaFormat::of::<Verdict>("verdict");
        assert_eq!(format.name, "verdict");
        let schema: Value = serde_json::from_str(&format.schema).expect("schema is JSON");
        assert_eq!(schema["properties"]["verdict"]["type"], json!("string"));
        assert_eq!(schema["required"], json!(["verdict"]));
    }

    #[test]
    fn function_of_type() {
        let function = Function::of::<Verdict>("judge", "judge a claim");
        assert_eq!(function.name, "judge");
        let schema: Value = serde_json::from_str(&function.parameters).expect("schema is JSON");
        assert!(schema["properties"]["verdict"].is_object());
    }

    #[test]
    fn tool_call_arguments() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "judge".to_owned(),
            arguments: r#"{"verdict":"pass"}"#.to_owned(),
        };
        assert_eq!(call.arguments::<Verdict>().expect("valid arguments").verdict, "pass");
        let error = call.arguments::<Vec<u8>>().expect_err("an object is not an array");
        assert!(error.starts_with("invalid arguments"), "{error}");
    }

    #[test]
    fn schema_hint() {
        let question = Question::<Verdict>::new("verdict")
            .schema(|schema| schema["properties"]["verdict"]["enum"] = json!(["pass", "fail"]));
        let Format::Schema(spec) = &question.request().format else {
            panic!("a question steers by schema");
        };
        let schema: Value = serde_json::from_str(&spec.schema).expect("schema is JSON");
        assert_eq!(schema["properties"]["verdict"]["enum"], json!(["pass", "fail"]));
        assert!(question.request().check);
    }

    #[test]
    fn correction_template() {
        let text =
            default_correction("{}", &["missing verdict".to_owned(), "too short".to_owned()]);
        assert!(text.starts_with("## Previous answer (rejected)\n\n{}\n\n## Findings\n\n"));
        assert!(text.contains("missing verdict\ntoo short"), "{text}");
    }
}
