//! `ReadyCheck` runs its closure fresh each call and reports the closure's verdict.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn ok_check_reports_ready() {
    let check = ReadyCheck::new("cache", || async { Ok(()) });
    assert_eq!(check.name(), "cache");
    assert!(check.run().await.is_ok());
}

#[tokio::test]
async fn failing_check_carries_its_error_string() {
    let check = ReadyCheck::new("downstream", || async { Err("peer unreachable".to_string()) });
    assert_eq!(check.run().await.unwrap_err(), "peer unreachable");
}

#[tokio::test]
async fn check_is_invoked_fresh_every_run() {
    // Readiness is a LIVE check, not a cached verdict: each run re-invokes the closure.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let check = ReadyCheck::new("live", move || {
        let counted = counted.clone();
        async move {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });
    check.run().await.unwrap();
    check.run().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
