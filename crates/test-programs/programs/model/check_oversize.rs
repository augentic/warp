//! A correction over the host's byte cap fails the completion, like an
//! oversize tool result: the check's text is bound for the model too.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Error, Question, WasiModel};
use test_programs::Verdict;

omnia_guest::command!(scenario);

async fn scenario() {
    let error = Question::<Verdict>::new("verdict")
        .ask(&WasiModel, "judge this", None, Verdict::passing)
        .await
        .expect_err("the correction exceeds the cap");
    assert!(
        matches!(error, Error::ToolFailed(ref detail) if detail.contains("exceeds the 4-byte cap")),
        "unexpected: {error:?}"
    );
}
