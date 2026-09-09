//! The candidate never deserializes as `T`: the schema and the type
//! disagree, which the guest sees as `invalid-request`, not more rounds.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Error, Question, WasiModel};
use test_programs::Verdict;

omnia_guest::command!(scenario);

async fn scenario() {
    let error = Question::<Verdict>::new("verdict")
        .ask(&WasiModel, "judge this", None, |_: &Verdict| {
            panic!("a candidate that does not deserialize never reaches the check")
        })
        .await
        .expect_err("the candidate does not deserialize");
    let Error::InvalidRequest(detail) = error else {
        panic!("expected invalid-request, got {error:?}");
    };
    assert!(detail.contains("schema and answer type disagree"), "unexpected: {detail}");
}
