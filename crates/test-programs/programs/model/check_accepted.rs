//! A `Question<T>` round trip: the first candidate passes the guest's check
//! and comes back as a `T`.

#![cfg(target_arch = "wasm32")]

use omnia_guest::model::{Question, WasiModel};
use test_programs::Verdict;

omnia_guest::command!(scenario);

async fn scenario() {
    let verdict = Question::<Verdict>::new("verdict")
        .system("judge")
        .ask(&WasiModel, "judge this", None, Verdict::passing)
        .await
        .expect("the candidate passes the check");
    assert_eq!(
        verdict,
        Verdict {
            verdict: "pass".to_owned(),
            findings: vec![],
        }
    );
}
