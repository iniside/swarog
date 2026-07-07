// Command scheduler-svc is the scheduler microservice: it hosts ONLY the
// scheduler module. It imports no other module, so `go build ./cmd/scheduler-svc`
// links only this service's code path (Go's linker drops every package this
// binary never imports) — the "extractable to microservices" claim made concrete
// for a pure event producer.
//
// It needs no QUIC edge: scheduler exposes no synchronous RPC, it only PRODUCES
// events. So it passes a nil edge server to app.Run. Its outbox relay (driven by
// EVENTS_SUBSCRIBERS) POSTs scheduler.fired to the remote audit sink, which is
// how a split exercises the outbox delivery path end-to-end.
package main

import (
	"log/slog"
	"os"

	"gamebackend/internal/app"
	"gamebackend/lifecycle"
	"gamebackend/modules/scheduler"
)

func main() {
	mods := []lifecycle.Module{&scheduler.Module{}}

	// nil edge server: this process exposes no edge-backed services, only events.
	if err := app.Run(app.ConfigFromEnv(), mods, nil); err != nil {
		slog.New(slog.NewTextHandler(os.Stdout, nil)).Error("scheduler-svc exited", "err", err)
		os.Exit(1)
	}
}
