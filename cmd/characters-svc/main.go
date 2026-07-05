// Command characters-svc is the characters microservice: it hosts ONLY the
// accounts + characters modules. It imports neither inventory, admin, match,
// rating, leaderboard nor webui — so `go build ./cmd/characters-svc` links only
// this service's code path (Go's linker drops every package this binary never
// imports). This is the "prove it builds only what's needed" end of the split.
//
// It stands up a single shared QUIC edge server and injects it into both
// modules, which register their edge handlers (accounts.verifySession,
// characters.ownerOf) on it so a peer's inventory can resolve session + owner
// over the wire. The characters outbox relay (driven by EVENTS_SUBSCRIBERS)
// runs inside this process because it drains characters' own outbox table.
package main

import (
	"log/slog"
	"os"

	"gamebackend/edge"
	"gamebackend/internal/app"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts"
	"gamebackend/modules/characters"
)

func main() {
	// One shared QUIC edge server for the whole process; both providers register
	// their handlers on it (a single UDP port serves every edge method).
	srv := edge.NewServer()

	am := &accounts.Module{Edge: srv}
	cm := &characters.Module{Edge: srv}

	mods := []lifecycle.Module{am, cm}

	if err := app.Run(app.ConfigFromEnv(), mods, srv); err != nil {
		slog.New(slog.NewTextHandler(os.Stdout, nil)).Error("characters-svc exited", "err", err)
		os.Exit(1)
	}
}
