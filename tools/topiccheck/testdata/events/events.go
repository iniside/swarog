// Package events is a topiccheck test fixture: three defined topics exercising
// the subscribed / unsubscribed / allowlisted cases.
package events

import "gamebackend/bus"

// P is a throwaway payload for the fixture topics.
type P struct{}

// SubscribedEvent is subscribed by the sub package — must NOT be reported.
var SubscribedEvent = bus.Define[P]("testdata.subscribed")

// UnsubscribedEvent has no subscriber and no allowlist — must be reported.
var UnsubscribedEvent = bus.Define[P]("testdata.unsubscribed")

//topiccheck:allow-unsubscribed reason="fixture: intentionally unsubscribed"
var AllowlistedEvent = bus.Define[P]("testdata.allowlisted")
