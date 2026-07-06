package gateway

import (
	"net/http"
	"net/http/httputil"
	"net/url"
)

// NewHTTPProxy builds an http.Handler that reverse-proxies each request to the
// origin serving its path prefix. routes maps a ServeMux pattern (e.g.
// "/characters/", "/inventory/", "/admin/") to an origin host:port; the caller
// decides the prefixes. Each origin is mounted with a SingleHostReverseProxy
// whose target URL has no base path, so the request path is preserved verbatim
// to the backend.
func NewHTTPProxy(routes map[string]string) http.Handler {
	mux := http.NewServeMux()
	for pattern, originHostPort := range routes {
		proxy := httputil.NewSingleHostReverseProxy(&url.URL{Scheme: "http", Host: originHostPort})
		mux.Handle(pattern, proxy)
	}
	return mux
}
