use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

// The registration runs against the handed server exactly when applied — the
// mechanics every module's contributed closure relies on.
#[test]
fn apply_runs_the_registration_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let reg = EdgeReg::new(move |_s: &mut Server| {
        counted.fetch_add(1, Ordering::SeqCst);
    });

    let mut server = Server::new();
    reg.apply(&mut server);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A second apply is a no-op: the FnOnce was consumed.
    reg.apply(&mut server);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// The contrib registry returns contributions by CLONE — all clones must share the
// ONE one-shot closure, so an apply through any clone spends it for every clone.
#[test]
fn clones_share_the_single_shot_closure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let reg = EdgeReg::new(move |_s: &mut Server| {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let clone = reg.clone();

    let mut server = Server::new();
    clone.apply(&mut server);
    reg.apply(&mut server);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// The monolith path: a contributed registration that is never applied is simply
// dropped — no panic, no side effect. (The closure and its captured Arc<Service>
// just fall out of scope.)
#[test]
fn unapplied_registration_drops_silently() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    {
        let _reg = EdgeReg::new(move |_s: &mut Server| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
