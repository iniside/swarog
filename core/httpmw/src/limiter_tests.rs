//! Limiter unit tests: burst-then-block (rate-0 determinism, Go's
//! `TestRateLimit_AllowsBurstThenBlocks` at the bucket level), per-IP isolation,
//! refill, and idle eviction (Go's `TestEvictIdle`).

use super::*;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[test]
fn allows_exactly_burst_then_denies_with_zero_rate() {
    // rate 0 so no tokens refill: exactly `burst` pass, then denied (the determinism
    // trick Go's test uses).
    let burst = 3;
    let lim = IpLimiter::new(0.0, burst);
    let who = ip(9, 9, 9, 9);
    for i in 0..burst {
        assert!(lim.allow(who), "request {i} within burst should pass");
    }
    assert!(!lim.allow(who), "burst+1 should be denied");
}

#[test]
fn zero_burst_denies_everything() {
    let lim = IpLimiter::new(0.0, 0);
    assert!(!lim.allow(ip(9, 9, 9, 9)));
}

#[test]
fn positive_rate_zero_burst_still_denies_everything() {
    // Regression pin for Step 13 / DEFECT 1: the mechanism itself stays dumb (a
    // capacity-0 bucket denies unconditionally regardless of refill rate) — the
    // pair-policy gate belongs to `core/app::env_rate_pair`, not here. A nonzero
    // rate must not accidentally let a zero-capacity bucket admit anything.
    let lim = IpLimiter::new(100.0, 0);
    let who = ip(9, 9, 9, 8);
    assert!(!lim.allow(who));
    assert!(!lim.allow(who));
}

#[test]
fn buckets_are_per_ip() {
    let lim = IpLimiter::new(0.0, 1);
    let a = ip(1, 1, 1, 1);
    let b = ip(2, 2, 2, 2);
    assert!(lim.allow(a));
    assert!(!lim.allow(a), "a is exhausted");
    assert!(lim.allow(b), "b has its own fresh bucket");
}

#[test]
fn refills_over_time() {
    // rate 100/s, burst 1: spend it, then after ~50ms a token is back (100/s refills 1
    // token in 10ms, so 50ms is comfortably enough — not timing-fragile).
    let lim = IpLimiter::new(100.0, 1);
    let who = ip(3, 3, 3, 3);
    assert!(lim.allow(who));
    assert!(!lim.allow(who));
    std::thread::sleep(Duration::from_millis(50));
    assert!(lim.allow(who), "bucket should refill after a pause");
}

#[test]
fn evict_idle_reaps_only_stale_buckets() {
    // Go's TestEvictIdle: backdate one visitor beyond the window, keep the other fresh.
    let lim = IpLimiter::new(1.0, 1);
    let stale = ip(1, 1, 1, 1);
    let fresh = ip(2, 2, 2, 2);
    lim.allow(stale);
    lim.allow(fresh);
    lim.backdate(stale, EVICT_AFTER + Duration::from_secs(60));

    lim.evict_idle(Instant::now());

    assert!(!lim.has_bucket(stale), "stale visitor should have been evicted");
    assert!(lim.has_bucket(fresh), "fresh visitor should remain");
    assert_eq!(lim.bucket_count(), 1);
}

#[test]
fn visitor_cap_rejects_only_new_ips_until_reaper_frees_space() {
    let before = table_saturated_total().get();
    let lim = IpLimiter::with_max_visitors(0.0, 2, 2);
    let existing = ip(1, 1, 1, 1);
    let stale = ip(2, 2, 2, 2);
    let newcomer = ip(3, 3, 3, 3);

    assert!(lim.allow(existing));
    assert!(lim.allow(stale));
    assert_eq!(lim.bucket_count(), 2);

    // Existing entries still consult their bucket at capacity. A new address is
    // rejected without insertion, so the table cannot grow beyond its immutable cap.
    assert!(lim.allow(existing));
    assert!(!lim.allow(newcomer));
    assert!(!lim.has_bucket(newcomer));
    assert_eq!(lim.bucket_count(), 2);
    assert_eq!(table_saturated_total().get(), before + 1);

    // Capacity is recovered only by the existing reaper path.
    lim.backdate(stale, EVICT_AFTER + Duration::from_secs(1));
    lim.evict_idle(Instant::now());
    assert_eq!(lim.bucket_count(), 1);
    assert!(lim.allow(newcomer));
    assert!(lim.has_bucket(newcomer));
    assert_eq!(lim.bucket_count(), 2);
}
