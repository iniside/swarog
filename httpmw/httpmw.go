// Package httpmw holds cross-cutting HTTP middleware mounted in the boot layer
// (internal/app, cmd/gateway-svc) — never in a module. A module registers routes
// on ctx.Mux; only the boot layer, after every module has mounted, can wrap the
// WHOLE mux. It imports only stdlib + golang.org/x/time/rate — a leaf, same tier
// as bus/registry/contrib.
package httpmw

import (
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"golang.org/x/time/rate"
)

// visitor is one client's token bucket plus the last time we saw it, so the
// cleanup goroutine can evict idle buckets and keep the map bounded.
type visitor struct {
	limiter  *rate.Limiter
	lastSeen time.Time
}

// IPLimiter is a per-IP token-bucket rate limiter. Each distinct client IP gets
// its own *rate.Limiter (rate r, burst b); buckets unused for longer than the
// eviction window are reaped by a background goroutine so the map stays bounded.
type IPLimiter struct {
	mu       sync.Mutex
	visitors map[string]*visitor
	rate     rate.Limit
	burst    int
}

// evictAfter is how long a per-IP bucket may sit idle before the cleanup
// goroutine reaps it.
const evictAfter = 3 * time.Minute

// NewIPLimiter builds an IPLimiter handing every new IP a bucket of rate r and
// burst b, and starts the background cleanup goroutine that evicts buckets idle
// for longer than evictAfter. The goroutine runs for the process lifetime.
func NewIPLimiter(r rate.Limit, b int) *IPLimiter {
	l := &IPLimiter{
		visitors: make(map[string]*visitor),
		rate:     r,
		burst:    b,
	}
	go l.cleanupLoop()
	return l
}

// Allow reports whether a request from ip may proceed now, consuming one token
// from that IP's bucket (creating the bucket on first sight).
func (l *IPLimiter) Allow(ip string) bool {
	l.mu.Lock()
	v, ok := l.visitors[ip]
	if !ok {
		v = &visitor{limiter: rate.NewLimiter(l.rate, l.burst)}
		l.visitors[ip] = v
	}
	v.lastSeen = time.Now()
	lim := v.limiter
	l.mu.Unlock()
	return lim.Allow()
}

// cleanupLoop periodically evicts buckets idle longer than evictAfter.
func (l *IPLimiter) cleanupLoop() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for range ticker.C {
		l.evictIdle(time.Now())
	}
}

// evictIdle drops every bucket whose lastSeen is older than now-evictAfter. Split
// out from cleanupLoop so tests can drive eviction deterministically.
func (l *IPLimiter) evictIdle(now time.Time) {
	l.mu.Lock()
	for ip, v := range l.visitors {
		if now.Sub(v.lastSeen) > evictAfter {
			delete(l.visitors, ip)
		}
	}
	l.mu.Unlock()
}

// RateLimit wraps next with per-IP rate limiting. When skip(r) is true the
// request passes untouched (infra probes/scrape must never get a 429). Otherwise
// the client IP is derived via clientIP and, if its bucket is exhausted, the
// request is rejected with 429 and Retry-After: 1.
func RateLimit(l *IPLimiter, clientIP func(*http.Request) string, skip func(*http.Request) bool, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if skip != nil && skip(r) {
			next.ServeHTTP(w, r)
			return
		}
		if !l.Allow(clientIP(r)) {
			w.Header().Set("Retry-After", "1")
			http.Error(w, "rate limit exceeded", http.StatusTooManyRequests)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// ClientIP returns a function that extracts the trustworthy client IP from a
// request. The host of RemoteAddr is the ground truth (the kernel-observed peer);
// X-Forwarded-For is honored ONLY when RemoteAddr itself is a trusted proxy.
//
// SECURITY: when trusted, we walk XFF from the RIGHT, skipping trusted CIDRs, and
// take the first UNTRUSTED address — never index 0. Go's httputil.ReverseProxy
// APPENDS the real peer on the right, so XFF[0] is fully attacker-controlled (a
// fresh fake per request would mint a fresh bucket and bypass the limit). If no
// untrusted hop is found, fall back to X-Real-IP, then the RemoteAddr host.
func ClientIP(trusted []*net.IPNet) func(*http.Request) string {
	return func(r *http.Request) string {
		host := remoteHost(r.RemoteAddr)
		if !isTrusted(host, trusted) {
			// RemoteAddr is not a trusted proxy: it is the only IP we can
			// believe. Ignore any forwarding headers (spoofable).
			return host
		}
		// RemoteAddr is a trusted proxy: walk XFF right-to-left for the first
		// hop that is NOT one of our trusted proxies.
		if xff := r.Header.Get("X-Forwarded-For"); xff != "" {
			parts := strings.Split(xff, ",")
			for i := len(parts) - 1; i >= 0; i-- {
				ip := strings.TrimSpace(parts[i])
				if ip == "" {
					continue
				}
				if !isTrusted(ip, trusted) {
					return ip
				}
			}
		}
		if xr := strings.TrimSpace(r.Header.Get("X-Real-IP")); xr != "" {
			return xr
		}
		return host
	}
}

// remoteHost splits host:port from a RemoteAddr, tolerating a bare host.
func remoteHost(remoteAddr string) string {
	if h, _, err := net.SplitHostPort(remoteAddr); err == nil {
		return h
	}
	return remoteAddr
}

// isTrusted reports whether ip (a plain host, no port) parses and falls within
// any trusted CIDR.
func isTrusted(ip string, trusted []*net.IPNet) bool {
	parsed := net.ParseIP(ip)
	if parsed == nil {
		return false
	}
	for _, n := range trusted {
		if n.Contains(parsed) {
			return true
		}
	}
	return false
}

// ParseCIDRs parses a comma-separated list of CIDRs (TRUSTED_PROXY_CIDRS). Blank
// entries are skipped; an empty/whitespace input yields a nil slice, no error.
func ParseCIDRs(csv string) ([]*net.IPNet, error) {
	var out []*net.IPNet
	for _, part := range strings.Split(csv, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		_, n, err := net.ParseCIDR(part)
		if err != nil {
			return nil, err
		}
		out = append(out, n)
	}
	return out, nil
}

// SkipInfra reports whether r targets an infra endpoint that must never be rate
// limited: k8s liveness/readiness probes and the Prometheus scrape all arrive
// from one IP and a 429 there means a restart loop or scrape gaps.
func SkipInfra(r *http.Request) bool {
	switch r.URL.Path {
	case "/healthz", "/readyz", "/metrics":
		return true
	default:
		return false
	}
}
