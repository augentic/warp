//! The guest's check rejects the first candidate; the correction goes back
//! over the session and the second candidate is accepted.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Question, WasiModel};
use test_programs::Verdict;

omnia_guest::command!(scenario);

async fn scenario() {
    let mut candidates = 0;
    let verdict = Question::<Verdict>::new("verdict")
        .ask(&WasiModel, "judge this", None, |verdict: &Verdict| {
            candidates += 1;
            verdict.passing()
        })
        .await
        .expect("the second candidate passes the check");
    assert_eq!(verdict.verdict, "pass");
    assert_eq!(candidates, 2, "both candidates reached the guest's check");
}
