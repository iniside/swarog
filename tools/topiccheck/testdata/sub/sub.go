// Package sub is a topiccheck test fixture: it subscribes to the events package's
// topics through all three subscribe funcs (bus.On, bus.OnTx, bus.OnTxRaw), so the
// analyzer can prove each wiring style is detected.
package sub

import (
	"context"
	"database/sql"
	"encoding/json"

	"gamebackend/bus"
	ev "gamebackend/tools/topiccheck/testdata/events"
)

// Wire subscribes to SubscribedEvent (bus.On), OnTxEvent (bus.OnTx, EventType var
// object identity), and OnTxRawEvent (bus.OnTxRaw, string-literal topic). It leaves
// UnsubscribedEvent and AllowlistedEvent untouched.
func Wire(b *bus.Bus) {
	bus.On(b, ev.SubscribedEvent, func(ev.P) {})
	bus.OnTx(b, ev.OnTxEvent, "sub", func(context.Context, *sql.Tx, ev.P) error { return nil })
	bus.OnTxRaw(b, "testdata.ontxraw", "sub", func(context.Context, *sql.Tx, json.RawMessage) error { return nil })
}
