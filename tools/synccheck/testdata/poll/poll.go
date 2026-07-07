// Package poll is the positive synccheck fixture: an async bus.Emit whose effect
// is then awaited by a busy-wait DB-poll loop (sleep + query). This is exactly
// the emit-then-poll smell the detector must FIRE on.
package poll

import (
	"database/sql"
	"time"

	"gamebackend/bus"
)

type payload struct{}

var thingEvent = bus.Define[payload]("synccheck.poll")

// Poll fires the event, then busy-waits in a for{ sleep; read } loop for its
// effect — the disguised-sync-RPC anti-pattern. MUST be flagged.
func Poll(b *bus.Bus, db *sql.DB) {
	bus.Emit(b, thingEvent, payload{})
	for {
		time.Sleep(10 * time.Millisecond)
		if db.QueryRow("SELECT 1").Err() == nil {
			return
		}
	}
}
