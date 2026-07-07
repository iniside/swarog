package httpmw

import (
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"golang.org/x/time/rate"
)

// okHandler is a trivial next-handler that records how many times it ran.
func okHandler(hits *int) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		*hits++
		w.WriteHeader(http.StatusOK)
	})
}

func TestRateLimit_AllowsBurstThenBlocks(t *testing.T) {
	const burst = 3
	// rate 0 so no tokens refill during the test: exactly `burst` pass, then 429.
	lim := NewIPLimiter(0, burst)
	hits := 0
	h := RateLimit(lim, func(*http.Request) string { return "9.9.9.9" }, nil, okHandler(&hits))

	for i := 0; i < burst; i++ {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, "/x", nil)
		h.ServeHTTP(rec, req)
		if rec.Code != http.StatusOK {
			t.Fatalf("request %d: got %d, want 200", i, rec.Code)
		}
	}

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/x", nil)
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusTooManyRequests {
		t.Fatalf("burst+1: got %d, want 429", rec.Code)
	}
	if got := rec.Header().Get("Retry-After"); got != "1" {
		t.Fatalf("Retry-After: got %q, want %q", got, "1")
	}
	if hits != burst {
		t.Fatalf("next-handler hits: got %d, want %d", hits, burst)
	}
}

func TestClientIP_AntiSpoof(t *testing.T) {
	trusted, err := ParseCIDRs("10.0.0.0/8")
	if err != nil {
		t.Fatal(err)
	}
	clientIP := ClientIP(trusted)

	// RemoteAddr is a trusted proxy; XFF ends with a trusted hop appended by the
	// proxy. The attacker-controlled left ("1.2.3.4") must be IGNORED; we take the
	// rightmost UNTRUSTED hop.
	req := httptest.NewRequest(http.MethodGet, "/x", nil)
	req.RemoteAddr = "10.0.0.1:5555"
	req.Header.Set("X-Forwarded-For", "1.2.3.4, 203.0.113.7, 10.0.0.9")
	if got := clientIP(req); got != "203.0.113.7" {
		t.Fatalf("trusted proxy: got %q, want %q (must not be 1.2.3.4)", got, "203.0.113.7")
	}

	// When ALL XFF hops are trusted, fall back to X-Real-IP.
	req2 := httptest.NewRequest(http.MethodGet, "/x", nil)
	req2.RemoteAddr = "10.0.0.1:5555"
	req2.Header.Set("X-Forwarded-For", "10.0.0.5, 10.0.0.9")
	req2.Header.Set("X-Real-IP", "198.51.100.2")
	if got := clientIP(req2); got != "198.51.100.2" {
		t.Fatalf("all-trusted XFF: got %q, want X-Real-IP %q", got, "198.51.100.2")
	}
}

func TestClientIP_UntrustedRemoteAddrIgnoresXFF(t *testing.T) {
	trusted, err := ParseCIDRs("10.0.0.0/8")
	if err != nil {
		t.Fatal(err)
	}
	clientIP := ClientIP(trusted)

	// RemoteAddr is NOT trusted → forwarding headers are spoofable, ignore them.
	req := httptest.NewRequest(http.MethodGet, "/x", nil)
	req.RemoteAddr = "203.0.113.50:4444"
	req.Header.Set("X-Forwarded-For", "1.2.3.4")
	req.Header.Set("X-Real-IP", "5.6.7.8")
	if got := clientIP(req); got != "203.0.113.50" {
		t.Fatalf("untrusted RemoteAddr: got %q, want %q", got, "203.0.113.50")
	}
}

func TestSkipInfra(t *testing.T) {
	// A limiter with zero capacity: without skip, everything is blocked.
	lim := NewIPLimiter(0, 0)
	hits := 0
	h := RateLimit(lim, func(*http.Request) string { return "9.9.9.9" }, SkipInfra, okHandler(&hits))

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("/metrics: got %d, want 200 (skipped)", rec.Code)
	}
	if hits != 1 {
		t.Fatalf("/metrics next hits: got %d, want 1", hits)
	}

	// A non-infra path with the same exhausted limiter is blocked.
	rec2 := httptest.NewRecorder()
	req2 := httptest.NewRequest(http.MethodGet, "/characters", nil)
	h.ServeHTTP(rec2, req2)
	if rec2.Code != http.StatusTooManyRequests {
		t.Fatalf("/characters: got %d, want 429", rec2.Code)
	}
}

func TestEvictIdle(t *testing.T) {
	lim := NewIPLimiter(rate.Limit(1), 1)
	lim.Allow("1.1.1.1")
	lim.Allow("2.2.2.2")

	// Backdate one visitor beyond the eviction window, keep the other fresh.
	lim.mu.Lock()
	lim.visitors["1.1.1.1"].lastSeen = time.Now().Add(-evictAfter - time.Minute)
	lim.mu.Unlock()

	lim.evictIdle(time.Now())

	lim.mu.Lock()
	_, stale := lim.visitors["1.1.1.1"]
	_, fresh := lim.visitors["2.2.2.2"]
	lim.mu.Unlock()
	if stale {
		t.Fatal("stale visitor 1.1.1.1 should have been evicted")
	}
	if !fresh {
		t.Fatal("fresh visitor 2.2.2.2 should remain")
	}
}
