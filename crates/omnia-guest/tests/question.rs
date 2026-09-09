//! `Question<T>` over a scripted model: the guest judges each candidate.

#![cfg(all(not(target_arch = "wasm32"), feature = "schema"))]

use omnia_guest::model::{Error, Question, ToolCall, Tools};
use omnia_guest::schemars::JsonSchema;
use omnia_test::guest::Scripted;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[schemars(crate = "omnia_guest::schemars")]
struct Verdict {
    verdict: String,
    findings: Vec<String>,
}

fn question() -> Question<Verdict> {
    Question::new("verdict").system("judge")
}

// Passes only a "pass" verdict with no findings.
fn strict(verdict: &Verdict) -> Result<(), Vec<String>> {
    let mut findings = Vec::new();
    if verdict.verdict != "pass" {
        findings.push("verdict must be `pass`".to_owned());
    }
    if !verdict.findings.is_empty() {
        findings.push("findings must be empty".to_owned());
    }
    if findings.is_empty() { Ok(()) } else { Err(findings) }
}

#[tokio::test]
async fn accepted() {
    let model = Scripted::answering([r#"{"verdict":"pass","findings":[]}"#]);
    let verdict = question().ask(&model, "judge this", None, strict).await.expect("a verdict");
    assert_eq!(verdict.verdict, "pass");

    let seen = model.seen().remove(0);
    assert!(seen.check);
    assert_eq!(seen.system.as_deref(), Some("judge"));
    assert_eq!(seen.messages, ["judge this"]);
    assert_eq!(model.exchanges()[0].tool, "check");
}

#[tokio::test]
async fn corrected() {
    let model = Scripted::answering([
        r#"{"verdict":"fail","findings":["x"]}"#,
        r#"{"verdict":"pass","findings":[]}"#,
    ]);
    let verdict = question().ask(&model, "judge this", None, strict).await.expect("a verdict");
    assert_eq!(verdict.verdict, "pass");

    let exchanges = model.exchanges();
    assert_eq!(exchanges.len(), 2);
    let correction = exchanges[0].outcome.clone().expect_err("the first candidate is rejected");
    assert!(correction.contains("## Previous answer (rejected)"), "{correction}");
    assert!(correction.contains(r#"{"verdict":"fail","findings":["x"]}"#), "{correction}");
    assert!(correction.contains("## Findings\n\nverdict must be `pass`\nfindings must be empty"));
    assert_eq!(exchanges[1].outcome, Ok(String::new()));
}

#[tokio::test]
async fn exhausted() {
    let model = Scripted::answering([r#"{"verdict":"fail","findings":[]}"#]);
    let error = question().ask(&model, "judge this", None, strict).await.expect_err("rejected");
    let Error::BudgetExhausted(correction) = error else {
        panic!("expected budget-exhausted, got {error:?}");
    };
    assert!(correction.contains("verdict must be `pass`"), "{correction}");
}

#[tokio::test]
async fn mismatch() {
    // The candidate is valid JSON of the wrong shape: schema and `T` disagree.
    let model = Scripted::answering([r#"{"other":1}"#]);
    let error = question().ask(&model, "judge this", None, strict).await.expect_err("mismatch");
    let Error::InvalidRequest(detail) = error else {
        panic!("expected invalid-request, got {error:?}");
    };
    assert!(detail.contains("schema and answer type disagree"), "{detail}");
    assert!(detail.contains("missing field"), "{detail}");
}

#[tokio::test]
async fn mismatch_then_accepted_is_the_models_miss() {
    let model = Scripted::answering([r#"{"other":1}"#, r#"{"verdict":"pass","findings":[]}"#]);
    let verdict = question().ask(&model, "judge this", None, strict).await.expect("a verdict");
    assert_eq!(verdict.verdict, "pass");
}

#[tokio::test]
async fn unchecked_backend() {
    // A backend that ignores `check` returns a reply nothing accepted.
    let model = Unchecked(Scripted::answering([r#"{"verdict":"pass","findings":[]}"#]));
    let error =
        question().ask(&model, "judge this", None, strict).await.expect_err("no accepted answer");
    assert!(matches!(error, Error::Backend(_)), "{error:?}");
}

#[tokio::test]
async fn other_tools_reach_the_callers_handler() {
    let model = Scripted::answering([r#"{"verdict":"pass","findings":[]}"#]).calling(
        0,
        [ToolCall {
            id: "call-1".into(),
            name: "lookup".into(),
            arguments: r#"{"id":1}"#.into(),
        }],
    );
    let tools: Tools = Box::new(|call: ToolCall| {
        Box::pin(async move { Ok(format!("looked up {}", call.arguments)) })
    });
    question().ask(&model, "judge this", Some(tools), strict).await.expect("a verdict");
    let exchanges = model.exchanges();
    assert_eq!(exchanges[0].tool, "lookup");
    assert_eq!(exchanges[0].outcome, Ok(r#"looked up {"id":1}"#.into()));
    assert_eq!(exchanges[1].tool, "check");
}

#[tokio::test]
async fn no_tools_refuses_a_tool_call() {
    let model = Scripted::answering([r#"{"verdict":"pass","findings":[]}"#]).calling(
        0,
        [ToolCall {
            id: "call-1".into(),
            name: "lookup".into(),
            arguments: "{}".into(),
        }],
    );
    question().ask(&model, "judge this", None, strict).await.expect("a verdict");
    let refusal = model.exchanges()[0].outcome.clone().expect_err("no handler");
    assert!(refusal.contains("tool `lookup` has no handler"), "{refusal}");
}

/// A model that strips `check` before delegating: a backend ignoring the flag.
struct Unchecked(Scripted);

impl omnia_guest::Model for Unchecked {
    fn complete(
        &self, request: omnia_guest::model::Request,
    ) -> impl Future<Output = Result<omnia_guest::model::Reply, Error>> + Send {
        self.0.complete(request)
    }

    fn complete_with<H, F>(
        &self, request: omnia_guest::model::Request, handler: H,
    ) -> impl Future<Output = Result<omnia_guest::model::Reply, Error>> + Send
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send,
    {
        self.0.complete_with(
            omnia_guest::model::Request {
                check: false,
                ..request
            },
            handler,
        )
    }
}
