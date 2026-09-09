//! Every candidate is rejected: the backend's round budget ends the
//! completion as `budget-exhausted` carrying the guest's own correction.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Error, Question, WasiModel};
use test_programs::Verdict;

omnia_guest::command!(scenario);

async fn scenario() {
    let error = Question::<Verdict>::new("verdict")
        .ask(&WasiModel, "judge this", None, Verdict::passing)
        .await
        .expect_err("no candidate passes the check");
    let Error::BudgetExhausted(correction) = error else {
        panic!("expected budget-exhausted, got {error:?}");
    };
    assert!(correction.contains("## Previous answer (rejected)"), "unexpected: {correction}");
    assert!(correction.contains("verdict must be `pass`"), "unexpected: {correction}");
}
