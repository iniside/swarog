package gateway

import (
	"net/http"
	"net/http/httputil"
	"net/url"
)

// NewHTTPProxy builds an http.Handler that reverse-proxies each request to the
// origin serving its path prefix. routes maps a BARE path prefix (e.g.
// "/characters", "/inventory", "/admin") to an origin host:port; the caller
// decides the prefixes. Each prefix is registered at BOTH the exact path and the
// subtree ("/characters" and "/characters/"), so a request to the bare prefix is
// proxied verbatim with no trailing-slash redirect — the backend, which serves
// the exact path (e.g. "GET /characters"), receives it unchanged. Each origin is
// mounted with a SingleHostReverseProxy whose target URL has no base path, so the
// request path is preserved verbatim to the backend.
func NewHTTPProxy(routes map[string]string) http.Handler {
	mux := http.NewServeMux()
	for prefix, originHostPort := range routes {
		proxy := httputil.NewSingleHostReverseProxy(&url.URL{Scheme: "http", Host: originHostPort})
		mux.Handle(prefix, proxy)     // exact, e.g. "/characters"
		mux.Handle(prefix+"/", proxy) // subtree, e.g. "/characters/{id}"
	}
	return mux
}
