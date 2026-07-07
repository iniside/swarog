package match

import (
	"context"
	"log/slog"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/match/matchevents"
	"gamebackend/registry"
)

// ratingService is the SLICE of "rating" this module actually needs. Declaring
// it locally means match depends on a capability, not on the rating package.
type ratingService interface {
	MMR(playerID string) int
}

// Module is a POINTER receiver: it holds the resolved rating service + bus +
// logger so Report (an operation invoked by the gateway's LocalOp) can reach
// them without a closure captured in Init.
type Module struct {
	rs  ratingService
	bus *bus.Bus
	log *slog.Logger
}

func (*Module) Name() string       { return "match" }
func (*Module) Requires() []string { return []string{"rating"} } // needs a synchronous answer

// Register offers this module under its own name so the gateway's
// selectBackend (providerOf("match.report") == "match") resolves it to the
// LocalBackend in-process — the same registry-presence check every
// operation-migrated provider uses. It runs in Build's phase 1, before any
// Init; m.rs/m.bus/m.log are set in Init but Report is only called after Init
// completes.
func (m *Module) Register(ctx *lifecycle.Context) error {
	registry.Provide(ctx.Registry, "match", m)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.rs = registry.Require[ratingService](ctx.Registry, "rating") // assert to our local interface
	m.bus = ctx.Bus
	m.log = ctx.Log

	registerOps(ctx, m)
	return nil
}

// Report implements matchapi.Match: it records the result. The MMR read is
// SYNCHRONOUS (query rating right now, for the log line); the announcement is
// fire-and-forget (bus.Emit) — whoever cares about a finished match
// subscribes, match/rating never gets edited to add a listener.
func (m *Module) Report(_ context.Context, winner, loser string) error {
	m.log.Info("match reported",
		"winner", winner, "winnerMMR", m.rs.MMR(winner), "loser", loser)

	bus.Emit(m.bus, matchevents.FinishedEvent,
		matchevents.Finished{Winner: winner, Loser: loser})
	return nil
}
