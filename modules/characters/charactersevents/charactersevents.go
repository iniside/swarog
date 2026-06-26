// Package charactersevents is the published event vocabulary of the "characters"
// domain. Anyone reacting to character lifecycle imports this; nobody imports the
// characters implementation. Depends only on the core foundation.
package charactersevents

import "gamebackend/core"

// Created fires when a player creates a character.
type Created struct {
	CharacterID string
	PlayerID    string
	Name        string
	Class       string
}

// Deleted fires when a character is removed. Consumers (e.g. inventory) use it to
// clean up their own data for that character — no cross-module foreign key needed.
type Deleted struct {
	CharacterID string
	PlayerID    string
}

var (
	CreatedEvent = core.Define[Created]("character.created")
	DeletedEvent = core.Define[Deleted]("character.deleted")
)
