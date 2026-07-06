// Package accountsevents is the published event vocabulary of the "accounts"
// domain. Anyone who reacts to player lifecycle imports this; nobody imports the
// accounts implementation. Depends only on the core foundation.
package accountsevents

import "gamebackend/bus"

// PlayerRegistered fires the first time an identity provisions a new player —
// for any provider (dev today, epic/steam later). It carries our product-scoped
// player id, not any provider's external id.
type PlayerRegistered struct {
	PlayerID    string
	DisplayName string
	Provider    string
}

// PlayerRegisteredEvent binds the topic to its payload in one place.
var PlayerRegisteredEvent = bus.Define[PlayerRegistered]("player.registered")
