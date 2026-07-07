// Command inventory-svc is the inventory microservice: it hosts the inventory +
// admin modules and fills their accounts/characters dependencies with remote
// stubs that call the peer (characters-svc) over the QUIC edge. It imports the
// inventory + admin + remote packages and the shared CONTRACT packages
// (charactersevents / adminapi) — but NOT the accounts or characters IMPL, and
// not match/rating/leaderboard/webui at all. `go build ./cmd/inventory-svc`
// therefore links only this service's code path plus those tiny contracts.
package main

import (
	"log/slog"
	"os"
	"strings"

	"gamebackend/edge"
	"gamebackend/internal/app"
	"gamebackend/lifecycle"
	"gamebackend/modules/admin"
	"gamebackend/modules/audit"
	"gamebackend/modules/config"
	"gamebackend/modules/gateway"
	"gamebackend/modules/inventory"
	"gamebackend/modules/messaging"
	"gamebackend/modules/remote"
)

// peerEdgeAddr returns the QUIC edge address a remote stub for `name` should
// dial: env <NAME>_EDGE_ADDR (e.g. CHARACTERS_EDGE_ADDR), else the shared
// default. Both providers live behind the peer's single edge server, so both
// default to the same host:port.
func peerEdgeAddr(name string) string {
	if v := strings.TrimSpace(os.Getenv(strings.ToUpper(name) + "_EDGE_ADDR")); v != "" {
		return v
	}
	return "localhost:9000"
}

// peerAdminURL returns the peer's /admin-data/<name> HTTP URL a remote stub
// fetches its admin page from: env <NAME>_ADMIN_URL, else derived from the
// shared PEER_HTTP_ADDR base, else empty (⇒ the module contributes no admin
// item and simply doesn't appear in this process's /admin).
func peerAdminURL(name string) string {
	if v := strings.TrimSpace(os.Getenv(strings.ToUpper(name) + "_ADMIN_URL")); v != "" {
		return v
	}
	if base := strings.TrimSpace(os.Getenv("PEER_HTTP_ADDR")); base != "" {
		return "http://" + base + "/admin-data/" + name
	}
	return ""
}

func main() {
	// Remote stubs stand in for the peer-hosted providers: each Provides an
	// edge-backed client under the dependency name, so inventory's Require
	// resolves to a real QUIC caller across the process boundary. The admin URL
	// (when set) lets this process's /admin fan out to the peer's admin page.
	accStub := remote.NewStub("accounts", peerEdgeAddr("accounts"), peerAdminURL("accounts"))
	charStub := remote.NewStub("characters", peerEdgeAddr("characters"), peerAdminURL("characters"))

	// This process hosts its own QUIC edge server so a gateway can route
	// player-facing inventory reads ("inventory.list") to it.
	srv := edge.NewServer()

	mods := []lifecycle.Module{
		// front door: contributes the HTTP front-handler (passthrough today) via httpmw.FrontHandlerSlot
		&gateway.Module{},
		// central config: schema "config", provides "config", live-reload via LISTEN/NOTIFY
		&config.Module{},
		&inventory.Module{Edge: srv},
		&audit.Module{}, // event ledger + prune sink; scheduler-svc POSTs scheduler.fired here
		&admin.Module{},
		accStub,
		charStub,
		// messaging LAST: Register (phase 1) installs the durable transport before
		// inventory/audit's Init calls OnTx/OnTxRaw; registration order also
		// governs Stop, which runs in REVERSE — last-registered stops FIRST, so
		// delivery (and in-flight consume) halts before inventory/audit tear down.
		&messaging.Module{},
	}

	if err := app.Run(app.ConfigFromEnv(), mods, srv); err != nil {
		slog.New(slog.NewTextHandler(os.Stdout, nil)).Error("inventory-svc exited", "err", err)
		os.Exit(1)
	}
}
