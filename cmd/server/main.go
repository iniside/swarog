package main

import (
	"log/slog"
	"net/http"
	"os"

	"gamebackend/core"
	"gamebackend/modules/leaderboard"
	"gamebackend/modules/match"
	"gamebackend/modules/rating"
)

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	ctx := core.NewContext(log)
	reg := core.NewRegistry(ctx)

	// The ONLY place that knows the full module list. Adding a feature =
	// one line here + one new folder. Nothing else in the codebase changes.
	reg.Add(rating.Module{})
	reg.Add(leaderboard.Module{})
	reg.Add(match.Module{}) // order is free — topo-sort settles it

	if err := reg.Build(); err != nil {
		log.Error("startup failed", "err", err)
		os.Exit(1)
	}

	log.Info("listening", "addr", ":8080")
	if err := http.ListenAndServe(":8080", ctx.Mux); err != nil {
		log.Error("server stopped", "err", err)
		os.Exit(1)
	}
}
