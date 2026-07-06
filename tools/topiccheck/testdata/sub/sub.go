// Package sub is a topiccheck test fixture: it subscribes to exactly one of the
// events package's topics, so the object-identity match can prove it wired.
package sub

import (
	"gamebackend/bus"
	ev "gamebackend/tools/topiccheck/testdata/events"
)

// Wire subscribes to SubscribedEvent and nothing else.
func Wire(b *bus.Bus) {
	bus.On(b, ev.SubscribedEvent, func(ev.P) {})
}
