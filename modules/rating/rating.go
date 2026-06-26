package rating

import (
	"gamebackend/core"
	"gamebackend/modules/match/matchevents"
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

type Module struct{}

func (Module) Name() string        { return "rating" }
func (Module) DependsOn() []string { return nil } // foundation — depends on nobody

func (Module) Init(ctx *core.Context) error {
	svc := &Service{mmr: map[string]int{}}
	ctx.Provide("rating", svc)

	// rating ALSO reacts to match results — but via the bus, so "match" has
	// zero knowledge that "rating" exists.
	core.On(ctx.Bus, matchevents.FinishedEvent, func(r matchevents.Finished) {
		svc.setMMR(r.Winner, svc.MMR(r.Winner)+15)
		svc.setMMR(r.Loser, svc.MMR(r.Loser)-15)
	})
	return nil
}
