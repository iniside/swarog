// Package publisher is a negative synccheck fixture guarding the emit-in-loop
// case: a periodic publisher that reads then EMITS inside the loop (mirrors
// modules/config/listen.go). The loop body has both a sleep and a DB read AND a
// reachable emit — so only the emit-in-loop guard keeps it silent. MUST stay
// silent; if this fires, the guard is broken.
package publisher

import (
	"database/sql"
	"time"

	"gamebackend/bus"
)

type payload struct{}

var tickEvent = bus.Define[payload]("synccheck.publisher")

// Publish loops forever: back off, read the next batch, then emit. The emit is
// lexically inside the loop body, so this is a publisher, not a poll.
func Publish(b *bus.Bus, db *sql.DB) {
	for {
		time.Sleep(time.Second)
		if db.QueryRow("SELECT 1").Err() == nil {
			bus.Emit(b, tickEvent, payload{})
		}
	}
}
