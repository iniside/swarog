// Command server is the MONOLITH entrypoint: it imports and hosts every module
// in ONE binary. There is no ROLES gating and no remote stubs — everything runs
// in-process, so every cross-module dependency resolves locally (inventory's
// PlayerCharactersProvider takes the in-process branch, no edge server needed).
//
// The microservice entrypoints (cmd/characters-svc, cmd/inventory-svc) each
// import only their OWN modules, so `go build ./cmd/<svc>` links only that
// service's code path. This binary is the opposite end: the full set.
package main

import (
	"log/slog"
	"os"

	"gamebackend/internal/app"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts"
	"gamebackend/modules/admin"
	"gamebackend/modules/audit"
	"gamebackend/modules/characters"
	"gamebackend/modules/config"
	"gamebackend/modules/gateway"
	"gamebackend/modules/inventory"
	"gamebackend/modules/leaderboard"
	"gamebackend/modules/match"
	"gamebackend/modules/messaging"
	"gamebackend/modules/rating"
	"gamebackend/modules/scheduler"
	"gamebackend/modules/webui"
)

func main() {
	// All modules, hosted locally. Pointer receivers for the stateful ones
	// (db/verifiers/caches); value receivers for the stateless ones.
	mods := []lifecycle.Module{
		&gateway.Module{},     // front door: contributes the HTTP front-handler (passthrough today) via httpmw.FrontHandlerSlot
		&config.Module{},      // central DB-backed config: schema "config", provides "config", live-reload via LISTEN/NOTIFY
		&accounts.Module{},    // player identity; owns schema "accounts"
		&characters.Module{},  // depends on accounts
		&inventory.Module{},   // depends on accounts + characters
		&rating.Module{},      // provides the "rating" service
		&leaderboard.Module{}, // Postgres-backed match listener
		match.Module{},        // depends on rating
		webui.Module{},        // serves the SPA demo at "/"
		&scheduler.Module{},   // data-driven event source: schema "scheduler", emits scheduler.fired
		&audit.Module{},       // append-only event ledger: schema "audit", generic bus listener + prune
		&admin.Module{},       // GameOps portal at "/admin"
		// messaging LAST: Register (phase 1) installs the durable transport before
		// any consumer's Init, but registration ORDER here governs Stop, which runs
		// in REVERSE — last-registered stops FIRST, so delivery halts before any
		// producer/consumer module tears down its resources.
		&messaging.Module{},
	}

	// No edge server: every provider is in-process, so nothing crosses a QUIC
	// boundary in the monolith.
	if err := app.Run(app.ConfigFromEnv(), mods, nil); err != nil {
		slog.New(slog.NewTextHandler(os.Stdout, nil)).Error("monolith exited", "err", err)
		os.Exit(1)
	}
}
