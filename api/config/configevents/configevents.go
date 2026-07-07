// Package configevents is the published contract of the config domain: the
// config.changed event the listener fires after a setting write is observed.
// It is the ONLY surface other modules share with config (payload + descriptor).
package configevents

import "gamebackend/bus"

// Changed carries the namespaced setting that just changed and its new value.
// Evolve additively (constraint #6): add fields / a V2, never reshape.
type Changed struct {
	Namespace string
	Key       string
	Value     string
}

// ChangedEvent is the config.changed topic. The listener Emits it once the
// in-memory cache has been refreshed with the new value, so a subscriber that
// re-pulls via the config service sees the fresh value.
var ChangedEvent = bus.Define[Changed]("config.changed")
