package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"time"

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

	srv := &http.Server{Addr: ":8080", Handler: ctx.Mux}
	go func() {
		log.Info("listening", "addr", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown: stop accepting HTTP first (so no new events are
	// published), then drain the bus so no in-flight event is lost.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt)
	<-stop
	log.Info("shutting down")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(shutdownCtx); err != nil {
		log.Error("http shutdown", "err", err)
	}
	ctx.Bus.Close()
	log.Info("bye")
}
