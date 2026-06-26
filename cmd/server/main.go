package main

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib" // registers the "pgx" database/sql driver

	"gamebackend/core"
	"gamebackend/modules/accounts"
	"gamebackend/modules/leaderboard"
	"gamebackend/modules/match"
	"gamebackend/modules/rating"
	"gamebackend/modules/webui"
)

const defaultDSN = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	ctx := core.NewContext(log)

	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = defaultDSN
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		log.Error("open db", "err", err)
		os.Exit(1)
	}
	defer db.Close()
	pingCtx, cancelPing := context.WithTimeout(context.Background(), 5*time.Second)
	if err := db.PingContext(pingCtx); err != nil {
		cancelPing()
		log.Error("db unreachable", "err", err)
		os.Exit(1)
	}
	cancelPing()
	ctx.DB = db

	reg := core.NewRegistry(ctx)

	// The ONLY place that knows the full module list. Adding a feature =
	// one line here + one new folder. Nothing else in the codebase changes.
	reg.Add(&accounts.Module{})    // pointer: holds db + verifiers
	reg.Add(rating.Module{})
	reg.Add(&leaderboard.Module{}) // pointer: holds db + logger
	reg.Add(match.Module{})        // order is free — topo-sort settles it
	reg.Add(webui.Module{})        // serves the account-linking demo page at "/"

	if err := reg.Build(); err != nil {
		log.Error("startup failed", "err", err)
		os.Exit(1)
	}

	migCtx, cancelMig := context.WithTimeout(context.Background(), 30*time.Second)
	if err := reg.Migrate(migCtx, db); err != nil {
		cancelMig()
		log.Error("migrate failed", "err", err)
		os.Exit(1)
	}
	cancelMig()

	startCtx, cancelStart := context.WithTimeout(context.Background(), 10*time.Second)
	if err := reg.Start(startCtx); err != nil {
		cancelStart()
		log.Error("start failed", "err", err)
		os.Exit(1)
	}
	cancelStart()

	srv := &http.Server{Addr: ":8080", Handler: ctx.Mux}
	go func() {
		log.Info("listening", "addr", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown, in order:
	//   1. stop accepting HTTP (no new events get published),
	//   2. drain the bus (in-flight events finish while module resources are up),
	//   3. stop modules in reverse dependency order (close goroutines/resources).
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
	reg.Stop(shutdownCtx)
	log.Info("bye")
}
