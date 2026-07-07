// Package clean is a negative synccheck fixture: an async bus.Emit followed by
// an HTTP-style response and return, with no poll loop at all — the legitimate
// fire-and-forget shape (mirrors modules/match/match.go). MUST stay silent.
package clean

import (
	"net/http"

	"gamebackend/bus"
)

type payload struct{}

var doneEvent = bus.Define[payload]("synccheck.clean")

// Handle emits, then writes a response and returns. No loop, no poll.
func Handle(b *bus.Bus, w http.ResponseWriter) {
	bus.Emit(b, doneEvent, payload{})
	w.WriteHeader(http.StatusAccepted)
}
