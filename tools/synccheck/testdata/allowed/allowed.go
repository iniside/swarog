// Package allowed is a negative synccheck fixture: the same emit-then-poll shape
// as poll/, but carrying a //synccheck:allow directive on the enclosing func, so
// the suppression path MUST keep it silent.
package allowed

import (
	"database/sql"
	"time"

	"gamebackend/bus"
)

type payload struct{}

var thingEvent = bus.Define[payload]("synccheck.allowed")

//synccheck:allow reason="fixture: intentional"
func Poll(b *bus.Bus, db *sql.DB) {
	bus.Emit(b, thingEvent, payload{})
	for {
		time.Sleep(10 * time.Millisecond)
		if db.QueryRow("SELECT 1").Err() == nil {
			return
		}
	}
}
