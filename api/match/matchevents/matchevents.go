// Package matchevents is the published event vocabulary of the "match" domain.
// It is the only shared surface of this domain: anyone who reacts to a match
// imports this; nobody imports the match implementation. It depends only on the
// core foundation (for the EventType descriptor).
package matchevents

import "gamebackend/bus"

// Finished is the payload of the match-finished event. Treat it as published
// API: evolve it additively (new field / FinishedV2), never mutate the existing
// shape — a structural change here breaks every consumer at compile time.
type Finished struct {
	MatchID string
	Winner  string
	Loser   string
}

// FinishedEvent binds the topic to the Finished payload in one place. Publishers
// (bus.Emit) and subscribers (bus.On) both reference it, so topic and type
// can never drift apart.
var FinishedEvent = bus.Define[Finished]("match.finished")
