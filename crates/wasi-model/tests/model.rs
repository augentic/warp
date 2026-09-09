//! End-to-end tests for `omnia:model/completion`: every scenario runs a real
//! guest component from `crates/test-programs` through the omnia runtime
//! against an inline scenario backend. The guest asserts what it observes
//! across the boundary (and traps on failure); the host side asserts wire
//! fidelity and filesystem effects.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use omnia::{ExitStatus, Mount};
use omnia_test::host::{Backends, Deployment, ScriptedModel, scratch};
use omnia_test::{Exchange, SeenFormat};
use omnia_wasi_model::{
    Answer, FutureResult, Limits, ModelDefault, Request, ToolHost, Usage, WasiModel, WasiModelCtx,
};

// Every guest program in `crates/test-programs` must have a matching test
// here; a new program without one fails to compile.
test_programs::foreach_model!();

// ------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------

/// Run one guest program against `model`, requiring a clean exit.
async fn run_guest<M: WasiModelCtx + Clone>(wasm: &str, mounts: Vec<Mount>, model: M) {
    let backends = Backends::defaults().await.model(model);
    let status = Deployment::new()
        .guest("guest", wasm)
        .mounts(mounts)
        .run_host::<WasiModel, _>(backends)
        .await
        .expect("guest runs");
    assert_eq!(status, ExitStatus::SUCCESS, "guest `{wasm}` failed");
}

/// [`run_guest`] over a scripted model, also requiring the script to be
/// exactly consumed; returns the model for its recordings.
async fn run_scripted(wasm: &str, mounts: Vec<Mount>, model: ScriptedModel) -> ScriptedModel {
    run_guest(wasm, mounts, model.clone()).await;
    model.assert_exhausted();
    model
}

// ------------------------------------------------------------------------
// Scenario backends
// ------------------------------------------------------------------------
//
// Most scenarios script `omnia_test::host::ScriptedModel`; the two below
// exercise session behaviour a FIFO script cannot express and stay by hand.

/// Issues two `lookup` calls concurrently and answers with both outputs in
/// issue order, proving results correlate by id however the guest answers.
/// By hand: a script drives its steps serially.
#[derive(Clone, Copy, Debug)]
struct ParallelLookups;

impl WasiModelCtx for ParallelLookups {
    fn complete(&self, _request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer> {
        async move {
            let (first, second) = futures::join!(
                tool_host.call_tool("lookup".to_owned(), "1".to_owned()),
                tool_host.call_tool("lookup".to_owned(), "2".to_owned()),
            );
            let first = first?.map_err(|failure| anyhow::anyhow!("first call: {failure}"))?;
            let second = second?.map_err(|failure| anyhow::anyhow!("second call: {failure}"))?;
            Ok(format!("{first}|{second}").into())
        }
        .boxed()
    }
}

/// Calls an undeclared tool, ignores the hard failure, and still answers —
/// host enforcement must win over that `Ok`. By hand: a script propagates a
/// hard failure instead of swallowing it.
#[derive(Clone, Copy, Debug)]
struct IgnoringToolFailure;

impl WasiModelCtx for IgnoringToolFailure {
    fn complete(&self, _request: Request, tool_host: Arc<dyn ToolHost>) -> FutureResult<Answer> {
        async move {
            assert!(
                tool_host.call_tool("lookup".to_owned(), "{}".to_owned()).await.is_err(),
                "undeclared tool is a hard failure"
            );
            Ok("should not reach the guest".into())
        }
        .boxed()
    }
}

/// The candidates the `check_*` scenarios' scripted backend proposes.
const PASS: &str = r#"{"verdict":"pass","findings":[]}"#;
const FAIL: &str = r#"{"verdict":"fail","findings":["x"]}"#;

/// One recorded `check` round.
fn check(candidate: &str, outcome: Result<&str, &str>) -> Exchange {
    Exchange {
        tool: "check".into(),
        arguments: candidate.into(),
        outcome: outcome.map(str::to_owned).map_err(str::to_owned),
    }
}

/// The tool-call exchange every `lookup` scenario drives.
fn lookup(outcome: Result<&str, &str>) -> Exchange {
    Exchange {
        tool: "lookup".into(),
        arguments: "{}".into(),
        outcome: outcome.map(str::to_owned).map_err(str::to_owned),
    }
}

/// A `lookup` turn: one scripted call answered by the guest, then `answer`.
fn lookup_turn(answer: &str) -> ScriptedModel {
    ScriptedModel::answering([answer]).calling(0, [("lookup", "{}")])
}

/// The workspace turn every `workspace_*` scenario drives: read the seed,
/// write `out.txt`, list the root, answer.
fn workspace_turn() -> ScriptedModel {
    ScriptedModel::answering(["hello:out.txt,seed.txt"])
        .reading(0, "seed.txt")
        .writing(0, "out.txt", "written")
        .listing(0, "")
}

/// What a fully served [`workspace_turn`] records.
fn workspace_exchanges() -> Vec<Exchange> {
    let exchange = |tool: &str, path: &str, outcome: &str| Exchange {
        tool: tool.into(),
        arguments: path.into(),
        outcome: Ok(outcome.into()),
    };
    vec![
        exchange("read", "seed.txt", "hello"),
        exchange("write", "out.txt", ""),
        exchange("list", "", "out.txt,seed.txt"),
    ]
}

// ------------------------------------------------------------------------
// Scenarios (one per guest program; guest-side assertions live in
// `crates/test-programs/programs/model/`)
// ------------------------------------------------------------------------

#[tokio::test]
async fn model_echo_text() {
    let model =
        run_scripted(test_programs::MODEL_ECHO_TEXT, vec![], ScriptedModel::answering(["second"]))
            .await;

    // Wire fidelity: the request arrived at the backend intact.
    let seen = model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].system.as_deref(), Some("be terse"));
    assert_eq!(seen[0].messages, ["hi", "second"]);
    assert!(seen[0].tools.is_empty());
    assert!(seen[0].temperature.is_none());
}

#[tokio::test]
async fn model_request_shape() {
    let model =
        run_scripted(test_programs::MODEL_REQUEST_SHAPE, vec![], ScriptedModel::answering(["hi"]))
            .await;

    let seen = model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tools, ["docs"]);
    assert_eq!(seen[0].temperature, Some(0.25));
}

#[tokio::test]
async fn model_echo_json() {
    run_guest(test_programs::MODEL_ECHO_JSON, vec![], ModelDefault).await;
}

#[tokio::test]
async fn model_echo_schema_rejected() {
    run_guest(test_programs::MODEL_ECHO_SCHEMA_REJECTED, vec![], ModelDefault).await;
}

#[tokio::test]
async fn model_invalid_request() {
    // Every request is refused before the backend runs; an empty script
    // proves it is never reached.
    run_scripted(test_programs::MODEL_INVALID_REQUEST, vec![], ScriptedModel::default()).await;
}

#[tokio::test]
async fn model_schema_answer() {
    let model = ScriptedModel::answering([r#"{"verdict":"pass"}"#]);
    let model = run_scripted(test_programs::MODEL_SCHEMA_ANSWER, vec![], model).await;
    assert!(!model.seen()[0].check, "no check was asked for");
    assert!(model.exchanges().is_empty());
}

#[tokio::test]
async fn model_usage() {
    let answer = Answer {
        answer: "hi".to_owned(),
        usage: Some(Usage {
            input_tokens: 3,
            output_tokens: 5,
            reasoning_tokens: Some(1),
        }),
        transcript: None,
    };
    run_scripted(test_programs::MODEL_USAGE, vec![], ScriptedModel::replying([answer])).await;
}

#[tokio::test]
async fn model_check_accepted() {
    let model = ScriptedModel::answering([PASS]);
    let model = run_scripted(test_programs::MODEL_CHECK_ACCEPTED, vec![], model).await;

    // Wire fidelity: `check` and the type's schema crossed the boundary.
    let seen = model.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].check);
    assert_eq!(seen[0].system.as_deref(), Some("judge"));
    assert_eq!(seen[0].messages, ["judge this"]);
    let SeenFormat::Schema { name, schema } = &seen[0].format else {
        panic!("a question steers by schema: {:?}", seen[0].format);
    };
    assert_eq!(name, "verdict");
    assert!(schema.contains(r#""verdict""#) && schema.contains(r#""findings""#), "{schema}");
    assert_eq!(model.exchanges(), [check(PASS, Ok(""))]);
}

#[tokio::test]
async fn model_check_corrected() {
    let model = ScriptedModel::answering([FAIL, PASS]);
    let model = run_scripted(test_programs::MODEL_CHECK_CORRECTED, vec![], model).await;

    // The correction the guest's check produced went back over the session
    // verbatim, and the backend's second attempt carried the same request.
    assert_eq!(model.seen().len(), 2);
    let exchanges = model.exchanges();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0].tool, "check");
    assert_eq!(exchanges[0].arguments, FAIL);
    let correction = exchanges[0].outcome.clone().expect_err("the first candidate is rejected");
    assert!(correction.contains("## Previous answer (rejected)"), "{correction}");
    assert!(correction.contains(FAIL), "{correction}");
    assert!(correction.contains("## Findings\n\nverdict must be `pass`"), "{correction}");
    assert_eq!(exchanges[1], check(PASS, Ok("")));
}

#[tokio::test]
async fn model_check_exhausted() {
    let model = ScriptedModel::answering([FAIL, FAIL]);
    let model = run_scripted(test_programs::MODEL_CHECK_EXHAUSTED, vec![], model).await;
    let exchanges = model.exchanges();
    assert_eq!(exchanges.len(), 2, "every scripted candidate was offered");
    assert!(exchanges.iter().all(|exchange| exchange.outcome.is_err()));
}

#[tokio::test]
async fn model_check_mismatch() {
    // Valid JSON of the wrong shape: the guest's `T` refuses it every time.
    let model = ScriptedModel::answering([r#"{"other":1}"#]);
    let model = run_scripted(test_programs::MODEL_CHECK_MISMATCH, vec![], model).await;
    let correction = model.exchanges()[0].outcome.clone().expect_err("rejected");
    assert!(correction.contains("does not match the expected shape"), "{correction}");
}

#[tokio::test]
async fn model_check_plain() {
    let model = ScriptedModel::answering(["nope", "hi"]);
    let model = run_scripted(test_programs::MODEL_CHECK_PLAIN, vec![], model).await;
    assert_eq!(
        model.exchanges(),
        [check("nope", Err("say `hi`, not `nope`")), check("hi", Ok(""))]
    );

    // The echo default honours `check` too: its one candidate is the prompt.
    run_guest(test_programs::MODEL_CHECK_PLAIN, vec![], ModelDefault).await;
}

#[tokio::test]
async fn model_sections() {
    let user_turn = "review the Rust code\n\nthe Rust crate\n\nInput: in\nOutput: out";
    let model =
        run_scripted(test_programs::MODEL_SECTIONS, vec![], ScriptedModel::answering([user_turn]))
            .await;

    // The assembled system channel crossed the boundary intact.
    let seen = model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].system.as_deref(),
        Some("prefer {language}\n\na Rust reviewer\n\n- be Rust-idiomatic")
    );
}

#[tokio::test]
async fn model_tool_roundtrip() {
    let model = run_scripted(test_programs::MODEL_TOOL_ROUNDTRIP, vec![], lookup_turn("42")).await;
    assert_eq!(model.exchanges(), [lookup(Ok("42"))]);
}

#[tokio::test]
async fn model_tool_failure() {
    let model = run_scripted(
        test_programs::MODEL_TOOL_FAILURE,
        vec![],
        lookup_turn("tool failed: no data"),
    )
    .await;
    assert_eq!(model.exchanges(), [lookup(Err("no data"))]);
}

#[tokio::test]
async fn model_undeclared_tool() {
    run_guest(test_programs::MODEL_UNDECLARED_TOOL, vec![], IgnoringToolFailure).await;
}

#[tokio::test]
async fn model_tool_budget() {
    let model = ScriptedModel::answering(["42"])
        .calling(0, [("lookup", "{}"), ("lookup", "{}")])
        .limits(Limits {
            max_tool_calls: 1,
            ..Limits::default()
        });
    let model = run_scripted(test_programs::MODEL_TOOL_BUDGET, vec![], model).await;
    assert_eq!(model.exchanges(), [lookup(Ok("42"))], "the second call never reaches the guest");
}

#[tokio::test]
async fn model_tool_timeout() {
    let model = lookup_turn("42").limits(Limits {
        tool_timeout: Duration::from_millis(50),
        ..Limits::default()
    });
    let model = run_scripted(test_programs::MODEL_TOOL_TIMEOUT, vec![], model).await;
    assert!(model.exchanges().is_empty(), "the unanswered call fails hard");
}

#[tokio::test]
async fn model_tool_oversize() {
    let model = lookup_turn("42").limits(Limits {
        max_result_bytes: 4,
        ..Limits::default()
    });
    let model = run_scripted(test_programs::MODEL_TOOL_OVERSIZE, vec![], model).await;
    assert!(model.exchanges().is_empty(), "the oversize result fails hard");
}

#[tokio::test]
async fn model_results_closed() {
    let model = run_scripted(test_programs::MODEL_RESULTS_CLOSED, vec![], lookup_turn("42")).await;
    assert!(model.exchanges().is_empty(), "the closed stream fails hard");
}

#[tokio::test]
async fn model_stale_result() {
    let model = run_scripted(test_programs::MODEL_STALE_RESULT, vec![], lookup_turn("42")).await;
    assert_eq!(model.exchanges(), [lookup(Ok("42"))]);
}

#[tokio::test]
async fn model_out_of_order_results() {
    run_guest(test_programs::MODEL_OUT_OF_ORDER_RESULTS, vec![], ParallelLookups).await;
}

#[tokio::test]
async fn model_workspace_tools() {
    let workspace = scratch();
    fs::write(workspace.path().join("seed.txt"), "hello").expect("seeding workspace");

    let model = run_scripted(
        test_programs::MODEL_WORKSPACE_TOOLS,
        vec![workspace.mount(true)],
        workspace_turn(),
    )
    .await;

    // The host-injected tools read the seed and listed the root; the write
    // landed on the real filesystem, and `local_path` resolved to this mount.
    assert_eq!(model.exchanges(), workspace_exchanges());
    assert_eq!(fs::read_to_string(workspace.path().join("out.txt")).expect("out.txt"), "written");
    assert_eq!(model.lent(), [Some(workspace.path().to_path_buf())]);
}

#[tokio::test]
async fn model_workspace_denied() {
    // No mount and no grant: the host-injected tools must refuse to run.
    let model = run_scripted(test_programs::MODEL_WORKSPACE_DENIED, vec![], workspace_turn()).await;
    assert!(model.exchanges().is_empty(), "the first read fails hard");
    assert_eq!(model.lent(), [None]);
}

#[tokio::test]
async fn model_workspace_escape() {
    let workspace = scratch();
    run_scripted(
        test_programs::MODEL_WORKSPACE_ESCAPE,
        vec![workspace.mount(false)],
        ScriptedModel::default(),
    )
    .await;
}

#[tokio::test]
async fn model_workspace_subpath() {
    let workspace = scratch();
    let nested = workspace.path().join("nested");
    fs::create_dir(&nested).expect("creating nested dir");
    fs::write(nested.join("seed.txt"), "hello").expect("seeding nested workspace");

    let model = run_scripted(
        test_programs::MODEL_WORKSPACE_SUBPATH,
        vec![workspace.mount(true)],
        workspace_turn(),
    )
    .await;

    assert_eq!(model.exchanges(), workspace_exchanges());
    assert_eq!(fs::read_to_string(nested.join("out.txt")).expect("nested/out.txt"), "written");
    assert!(!workspace.path().join("out.txt").exists(), "write stays under the subpath");
    assert_eq!(model.lent(), [Some(nested)]);
}

#[tokio::test]
async fn model_workspace_readonly() {
    let workspace = scratch();
    fs::write(workspace.path().join("seed.txt"), "hello").expect("seeding workspace");
    let model = run_scripted(
        test_programs::MODEL_WORKSPACE_READONLY,
        vec![workspace.mount(false)],
        workspace_turn(),
    )
    .await;
    assert_eq!(model.exchanges(), [workspace_exchanges()[0].clone()], "the write fails hard");
}

#[tokio::test]
async fn model_workspace_unauthorized() {
    let workspace = scratch();
    fs::create_dir(workspace.path().join("nested")).expect("creating nested dir");
    run_scripted(
        test_programs::MODEL_WORKSPACE_UNAUTHORIZED,
        vec![workspace.mount(true)],
        ScriptedModel::default(),
    )
    .await;
}
