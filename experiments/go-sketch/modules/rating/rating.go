package rating

import (
	"gamebackend/api/match/matchevents"
	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/registry"
)

// Service is the contract this module provides via the registry.
type Service struct{ mmr map[string]int }

func (s *Service) MMR(playerID string) int {
	if v, ok := s.mmr[playerID]; ok {
		return v
	}
	return 1000 // default starting rating
}
func (s *Service) setMMR(id string, v int) { s.mmr[id] = v }

// Module is a POINTER receiver: it holds the constructed Service in a field so
// Register (which Provides it) and Init (which mutates it from the bus) share the
// same instance, instead of a value captured by a closure.
type Module struct{ svc *Service }

func (*Module) Name() string       { return "rating" }
func (*Module) Requires() []string { return nil } // foundation — depends on nobody

// Register builds the service and offers it to the registry in Build's phase 1,
// before any Init. The bus subscription that mutates it is wired in Init.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.svc = &Service{mmr: map[string]int{}}
	registry.Provide(ctx.Registry, "rating", m.svc)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	// rating ALSO reacts to match results — but via the bus, so "match" has
	// zero knowledge that "rating" exists.
	bus.On(ctx.Bus, matchevents.FinishedEvent, func(r matchevents.Finished) {
		m.svc.setMMR(r.Winner, m.svc.MMR(r.Winner)+15)
		m.svc.setMMR(r.Loser, m.svc.MMR(r.Loser)-15)
	})
	return nil
}
