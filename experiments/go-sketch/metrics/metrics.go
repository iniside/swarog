// Package metrics is the cross-cutting HTTP-metrics middleware mounted in the
// boot layer (internal/app only — the gateway stays lean, no prometheus). It
// owns a PRIVATE prometheus.Registry (never the global default), so scraping
// this process never picks up anything registered elsewhere and the gateway
// binary — which never imports this package — carries zero prometheus
// footprint. Same tier as httpmw: a leaf mounted once the whole mux is built.
package metrics

import (
	"bufio"
	"net"
	"net/http"
	"strconv"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// reg is this package's own registry — deliberately NOT prometheus.DefaultRegisterer,
// so metrics registered by other imported libraries (if any) never leak into our
// /metrics output and vice versa.
var reg = prometheus.NewRegistry()

var (
	requestsTotal = prometheus.NewCounterVec(
		prometheus.CounterOpts{
			Name: "http_requests_total",
			Help: "Total HTTP requests processed, labeled by method, route pattern, and status.",
		},
		[]string{"method", "path", "status"},
	)
	requestDuration = prometheus.NewHistogramVec(
		prometheus.HistogramOpts{
			Name:    "http_request_duration_seconds",
			Help:    "HTTP request latency in seconds, labeled by method, route pattern, and status.",
			Buckets: prometheus.DefBuckets,
		},
		[]string{"method", "path", "status"},
	)
)

func init() {
	reg.MustRegister(requestsTotal, requestDuration)
}

// statusRecorder wraps a ResponseWriter to capture the status code a handler
// wrote, defaulting to 200 (the value net/http assumes when a handler never
// calls WriteHeader). It delegates the optional http.Flusher/http.Hijacker
// interfaces to the wrapped ResponseWriter so SSE and connection hijacking
// keep working through this wrapper — the current mux doesn't use either, but
// a correct wrapper must not silently break them for a future handler that does.
type statusRecorder struct {
	http.ResponseWriter
	status int
}

func newStatusRecorder(w http.ResponseWriter) *statusRecorder {
	return &statusRecorder{ResponseWriter: w, status: http.StatusOK}
}

func (r *statusRecorder) WriteHeader(status int) {
	r.status = status
	r.ResponseWriter.WriteHeader(status)
}

// Flush delegates to the wrapped ResponseWriter's http.Flusher, if it
// implements one, so streaming responses aren't broken by this wrapper.
func (r *statusRecorder) Flush() {
	if f, ok := r.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

// Hijack delegates to the wrapped ResponseWriter's http.Hijacker, if it
// implements one, so connection hijacking (e.g. websockets) isn't broken by
// this wrapper.
func (r *statusRecorder) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	if h, ok := r.ResponseWriter.(http.Hijacker); ok {
		return h.Hijack()
	}
	return nil, nil, http.ErrNotSupported
}

// Middleware wraps next with HTTP request-count and latency metrics, labeled
// by method/path/status. It expects to wrap the WHOLE mux from the OUTSIDE of
// any rate limiter (metrics(ratelimit(mux))), so a 429 issued by the rate
// limiter is counted too.
//
// CRITICAL ordering: r.Pattern is read AFTER next.ServeHTTP returns, never
// before. ServeMux only populates r.Pattern while it matches the request —
// and the mux runs INSIDE next (this middleware wraps the mux, not the other
// way around) — so reading it beforehand would see an empty pattern for every
// request, making the path label useless for every route. An empty pattern
// after next has run means a real 404/no-match, not a bug; it is mapped to
// the fixed label "unmatched" so unmatched paths (attacker-supplied or
// otherwise) can never explode label cardinality.
func Middleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rec := newStatusRecorder(w)

		next.ServeHTTP(rec, r)

		elapsed := time.Since(start)
		path := r.Pattern
		if path == "" {
			path = "unmatched"
		}
		status := strconv.Itoa(rec.status)

		requestsTotal.WithLabelValues(r.Method, path, status).Inc()
		requestDuration.WithLabelValues(r.Method, path, status).Observe(elapsed.Seconds())
	})
}

// Handler serves this package's private registry in the Prometheus exposition
// format, for mounting at GET /metrics.
func Handler() http.Handler {
	return promhttp.HandlerFor(reg, promhttp.HandlerOpts{})
}
