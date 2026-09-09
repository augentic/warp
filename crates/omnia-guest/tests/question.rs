//! `Question<T>` over a scripted model: the guest judges each candidate.

#![cfg(not(target_arch = "wasm32"))]

use omnia_guest::model::{Error, Question, ToolCall, Tools};
use omnia_test::guest::Scripted;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
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
async fn own_correction_template() {
    let model = Scripted::answering([
        r#"{"verdict":"fail","findings":[]}"#,
        r#"{"verdict":"pass","findings":[]}"#,
    ]);
    let question = question()
        .correction(|previous, findings| format!("REJECTED {previous}: {}", findings.join("; ")));
    question.ask(&model, "judge this", None, strict).await.expect("a verdict");

    let correction = model.exchanges()[0].outcome.clone().expect_err("rejected");
    assert_eq!(correction, r#"REJECTED {"verdict":"fail","findings":[]}: verdict must be `pass`"#);
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
async fn mismatch_then_accepted() {
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
    let Error::Backend(detail) = error else {
        panic!("expected backend, got {error:?}");
    };
    assert_eq!(detail, "the backend finished without running the check");
}

#[tokio::test]
async fn heedless_backend() {
    // A backend that runs the check but finishes on the rejected candidate.
    let model = Heedless(r#"{"verdict":"fail","findings":[]}"#);
    let error =
        question().ask(&model, "judge this", None, strict).await.expect_err("no accepted answer");
    let Error::Backend(detail) = error else {
        panic!("expected backend, got {error:?}");
    };
    assert_eq!(detail, "the backend finished on a candidate the check rejected");
}

#[tokio::test]
async fn other_tools() {
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
async fn refuse_tool_call() {
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

/// A model that offers one candidate to `check` and replies with it whatever
/// the verdict: a backend that ignores the rejection.
struct Heedless(&'static str);

impl omnia_guest::Model for Heedless {
    fn complete(
        &self, _request: omnia_guest::model::Request,
    ) -> impl Future<Output = Result<omnia_guest::model::Reply, Error>> + Send {
        std::future::ready(Ok(self.reply()))
    }

    async fn complete_with<H, F>(
        &self, _request: omnia_guest::model::Request, mut handler: H,
    ) -> Result<omnia_guest::model::Reply, Error>
    where
        H: FnMut(ToolCall) -> F + Send,
        F: Future<Output = Result<String, String>> + Send,
    {
        let verdict = handler(ToolCall {
            id: "check-1".into(),
            name: "check".into(),
            arguments: self.0.into(),
        })
        .await;
        assert!(verdict.is_err(), "the scenario's candidate is one the check rejects");
        Ok(self.reply())
    }
}

impl Heedless {
    fn reply(&self) -> omnia_guest::model::Reply {
        omnia_guest::model::Reply {
            answer: self.0.to_owned(),
            usage: None,
        }
    }
}
