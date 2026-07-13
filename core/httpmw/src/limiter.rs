//! The per-IP token-bucket rate limiter (port of Go's `IPLimiter` over
//! `golang.org/x/time/rate`). Each distinct client IP gets its own bucket (rate `r`,
//! burst `b`); buckets idle longer than [`EVICT_AFTER`] are reaped by a background task
//! so the map stays bounded.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a per-IP bucket may sit idle before the cleanup task reaps it (Go's
/// `evictAfter`).
const EVICT_AFTER: Duration = Duration::from_secs(3 * 60);

/// Hard ceiling for distinct live client buckets. Idle entries leave only through the
/// existing minute reaper; a new address arriving at the ceiling is rejected in O(1).
const DEFAULT_MAX_VISITORS: usize = 65_536;

fn table_saturated_total() -> &'static prometheus::IntCounter {
    static COUNTER: OnceLock<prometheus::IntCounter> = OnceLock::new();
    COUNTER.get_or_init(|| {
        prometheus::IntCounter::new(
            "http_rate_limit_table_saturated_total",
            "HTTP requests rejected because the per-IP rate-limit table was full.",
        )
        .expect("valid http_rate_limit_table_saturated_total metric")
    })
}

/// Returns the rate-limit table saturation collector for registration in the process's
/// private metrics registry. The counter remains owned and updated by `httpmw`.
#[doc(hidden)]
pub fn table_saturation_collector() -> Box<dyn prometheus::core::Collector> {
    Box::new(table_saturated_total().clone())
}

/// A single token bucket, a faithful-enough port of `x/time/rate.Limiter`: `tokens`
/// refill continuously at `rate` per second up to `burst`, and each admitted request
/// spends one. Starts FULL (`tokens == burst`) so the first `burst` requests pass with
/// no refill — with `rate == 0` exactly `burst` pass, then every request is denied
/// (the determinism trick Go's tests rely on).
struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rate: f64, burst: f64, now: Instant) -> TokenBucket {
        TokenBucket {
            rate,
            burst,
            tokens: burst,
            last: now,
        }
    }

    /// Refills for the elapsed time (capped at `burst`), then admits one token if one
    /// is available. `now` is passed in so callers can share one clock read per request.
    fn allow_at(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One client's bucket plus the last time it was seen, so the cleanup task can evict
/// idle buckets and keep the map bounded.
struct Visitor {
    bucket: TokenBucket,
    last_seen: Instant,
}

/// A per-IP token-bucket rate limiter. Every distinct client IP gets its own bucket
/// (rate `r`, burst `b`); idle buckets are reaped by [`IpLimiter::spawn_eviction`].
///
/// Shared behind an `Arc` (the axum middleware and the eviction task both hold one);
/// the bucket map is behind a `Mutex` because [`IpLimiter::allow`] takes `&self` and is
/// hit concurrently by every request — same reason Go guarded its map with a mutex.
pub struct IpLimiter {
    visitors: Mutex<HashMap<IpAddr, Visitor>>,
    rate: f64,
    burst: f64,
    max_visitors: usize,
}

impl IpLimiter {
    /// Builds a limiter handing every new IP a bucket of rate `rate` (tokens/sec) and
    /// burst `burst`. Does NOT start the eviction task — call [`IpLimiter::spawn_eviction`]
    /// once inside an async context (the boot layer does; unit tests drive
    /// [`IpLimiter::evict_idle`] directly, deterministically, as Go's tests do).
    pub fn new(rate: f64, burst: u32) -> Arc<IpLimiter> {
        Self::with_capacity(rate, burst, DEFAULT_MAX_VISITORS)
    }

    fn with_capacity(rate: f64, burst: u32, max_visitors: usize) -> Arc<IpLimiter> {
        Arc::new(IpLimiter {
            visitors: Mutex::new(HashMap::new()),
            rate,
            burst: f64::from(burst),
            max_visitors,
        })
    }

    /// Test-only constructor with a tiny deterministic visitor ceiling.
    #[cfg(test)]
    pub(crate) fn with_max_visitors(
        rate: f64,
        burst: u32,
        max_visitors: usize,
    ) -> Arc<IpLimiter> {
        Self::with_capacity(rate, burst, max_visitors)
    }

    /// Reports whether a request from `ip` may proceed now, spending one token from that
    /// IP's bucket (creating the bucket, full, on first sight) and refreshing its
    /// last-seen time.
    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut visitors = self.visitors.lock().unwrap();

        if let Some(visitor) = visitors.get_mut(&ip) {
            visitor.last_seen = now;
            return visitor.bucket.allow_at(now);
        }

        if visitors.len() >= self.max_visitors {
            table_saturated_total().inc();
            return false;
        }

        let mut visitor = Visitor {
            bucket: TokenBucket::new(self.rate, self.burst, now),
            last_seen: now,
        };
        let allowed = visitor.bucket.allow_at(now);
        visitors.insert(ip, visitor);
        allowed
    }

    /// Drops every bucket idle longer than [`EVICT_AFTER`] as of `now`. Split from the
    /// loop so tests can drive eviction deterministically (Go's `evictIdle`).
    pub fn evict_idle(&self, now: Instant) {
        self.visitors
            .lock()
            .unwrap()
            .retain(|_, v| now.saturating_duration_since(v.last_seen) <= EVICT_AFTER);
    }

    /// Spawns the background task that evicts idle buckets once a minute for the process
    /// lifetime (Go's `cleanupLoop` goroutine). Requires a Tokio runtime — the boot
    /// layer calls it from inside `app::run`.
    pub fn spawn_eviction(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            // The immediate first tick fires instantly; skip acting on it — there is
            // nothing to evict at t0.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                this.evict_idle(Instant::now());
            }
        });
    }

    /// Test-only: how many buckets are currently held.
    #[cfg(test)]
    pub(crate) fn bucket_count(&self) -> usize {
        self.visitors.lock().unwrap().len()
    }

    /// Test-only: whether a bucket exists for `ip`.
    #[cfg(test)]
    pub(crate) fn has_bucket(&self, ip: IpAddr) -> bool {
        self.visitors.lock().unwrap().contains_key(&ip)
    }

    /// Test-only: backdate a bucket's last-seen so a subsequent [`IpLimiter::evict_idle`]
    /// reaps it (mirrors Go's test reaching into `visitors[ip].lastSeen`).
    #[cfg(test)]
    pub(crate) fn backdate(&self, ip: IpAddr, age: Duration) {
        if let Some(v) = self.visitors.lock().unwrap().get_mut(&ip) {
            v.last_seen = Instant::now() - age;
        }
    }
}

#[cfg(test)]
#[path = "limiter_tests.rs"]
mod limiter_tests;
