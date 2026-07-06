// Package webui serves the single-page account-linking demo. It is a UI-only
// module: no database, no services, no events — it just mounts a static page
// that talks to the accounts endpoints over fetch. It depends on nothing.
package webui

import (
	_ "embed"
	"net/http"

	"gamebackend/lifecycle"
)

//go:embed index.html
var indexHTML []byte

type Module struct{}

func (Module) Name() string       { return "webui" }
func (Module) Requires() []string { return nil }

func (Module) Init(ctx *lifecycle.Context) error {
	// "GET /" is the catch-all; more specific routes (e.g. "GET /accounts/me")
	// win, so this only serves the page itself and 404s everything else.
	ctx.Mux.HandleFunc("GET /", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.Write(indexHTML)
	})
	return nil
}
