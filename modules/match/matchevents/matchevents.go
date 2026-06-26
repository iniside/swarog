// Package matchevents is the published event vocabulary of the "match" domain —
// pure data, depending on NOTHING. It is the only shared surface of this domain:
// anyone who wants to react to a match imports this; nobody imports the
// match implementation.
package matchevents

const TopicFinished = "match.finished"

// Finished is the payload of the match-finished event. Treat it as published
// API: evolve it additively (new field / FinishedV2), never mutate the existing
// shape — a structural change here breaks every consumer at compile time.
type Finished struct {
	MatchID string
	Winner  string
	Loser   string
}
