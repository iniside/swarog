package metrics

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// TestMiddleware_RecordsRequestWithMatchedPattern drives a request through a
// real http.ServeMux (so r.Pattern is populated by the mux's own matching,
// wrapped by Middleware) and asserts the /metrics scrape reflects it under the
// expected method/path/status labels — proving Middleware reads r.Pattern
// AFTER next.ServeHTTP, not before (before would see it empty for every route).
func TestMiddleware_RecordsRequestWithMatchedPattern(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /widgets/{id}", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusCreated)
	})

	h := Middleware(mux)

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/widgets/42", nil)
	h.ServeHTTP(rec, req)

	if rec.Code != http.StatusCreated {
		t.Fatalf("got status %d, want %d", rec.Code, http.StatusCreated)
	}

	body := scrape(t)
	want := `http_requests_total{method="GET",path="GET /widgets/{id}",status="201"} 1`
	if !strings.Contains(body, want) {
		t.Fatalf("scrape missing %q\nfull body:\n%s", want, body)
	}
}

// TestMiddleware_UnmatchedRouteUsesFixedLabel asserts a request that never
// matches any route (a real 404) is labeled "unmatched" rather than leaking
// the raw, attacker-influenceable path into a label (cardinality guard).
func TestMiddleware_UnmatchedRouteUsesFixedLabel(t *testing.T) {
	mux := http.NewServeMux()
	h := Middleware(mux)

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/nope/does/not/exist", nil)
	h.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("got status %d, want %d", rec.Code, http.StatusNotFound)
	}

	body := scrape(t)
	want := `http_requests_total{method="GET",path="unmatched",status="404"}`
	if !strings.Contains(body, want) {
		t.Fatalf("scrape missing %q\nfull body:\n%s", want, body)
	}
}

// TestHandler_ServesPrometheusExposition asserts Handler() answers 200 with
// the standard Prometheus text exposition format.
func TestHandler_ServesPrometheusExposition(t *testing.T) {
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	Handler().ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("got status %d, want 200", rec.Code)
	}
	body := rec.Body.String()
	if !strings.Contains(body, "# HELP http_requests_total") {
		t.Fatalf("missing HELP line for http_requests_total:\n%s", body)
	}
	if !strings.Contains(body, "# TYPE http_requests_total counter") {
		t.Fatalf("missing TYPE line for http_requests_total:\n%s", body)
	}
}

// scrape hits Handler() and returns the full response body, failing the test
// on any read error.
func scrape(t *testing.T) string {
	t.Helper()
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	Handler().ServeHTTP(rec, req)
	b, err := io.ReadAll(rec.Body)
	if err != nil {
		t.Fatalf("read scrape body: %v", err)
	}
	return string(b)
}
