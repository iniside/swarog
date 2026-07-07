// Package schedulerevents is the published event vocabulary of the "scheduler"
// domain. When a named schedule's interval elapses the scheduler announces
// scheduler.fired{Name}; any module (e.g. audit's prune) reacts by subscribing.
// Nobody imports the scheduler implementation. Depends only on the core foundation.
package schedulerevents

import "gamebackend/bus"

// Fired announces that a named schedule's interval has elapsed — "it is time to
// do <Name>". The consumer decides what that means; the scheduler never knows
// who listens or what work the name implies (it is a shared vocabulary string,
// like a topic). Evolve additively (constraint #6).
type Fired struct {
	Name string
}

// FiredEvent binds the topic to its payload in one place.
//
// Delivery guarantees differ per topology: best-effort over the in-process bus
// in the monolith (a crash after the DB commit but before Emit loses the tick),
// at-least-once over the outbox relay in a split. Consumers MUST therefore be
// idempotent — see modules/scheduler/scheduler.go for the full rationale.
//
// The allow-unsubscribed comment is temporary: Step 5 (audit) subscribes to this
// topic via bus.On for its prune job, at which point the comment comes out. Until
// then topiccheck would otherwise flag a Define with no On.
//
//topiccheck:allow-unsubscribed reason="audit subscribes via bus.On in Step 5 (audit module); temporary"
var FiredEvent = bus.Define[Fired]("scheduler.fired")
