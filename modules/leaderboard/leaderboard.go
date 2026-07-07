package leaderboard

import (
	"context"
	"database/sql"
	"log/slog"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/leaderboard/leaderboardapi"
	"gamebackend/modules/match/matchevents"
	"gamebackend/registry"
)

// Module is Postgres-backed. It owns the "leaderboard" schema and nothing else —
// full logical isolation: no other module's tables, no cross-module foreign
// keys. The link to players is the bare player id carried by the event.
type Module struct {
	log *slog.Logger
	db  *sql.DB
}

func (*Module) Name() string       { return "leaderboard" }
func (*Module) Requires() []string { return nil } // reacts via the bus — depends on nobody

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS leaderboard;
CREATE TABLE IF NOT EXISTS leaderboard.scores (
	player text   PRIMARY KEY,
	wins   bigint NOT NULL DEFAULT 0
);`

// Migrate creates this module's own schema. Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Register offers this module under its own name so the gateway's selectBackend
// (providerOf("leaderboard.topScores") == "leaderboard") resolves it to the
// LocalBackend in-process — the same registry-presence check every
// operation-migrated provider uses. It runs in Build's phase 1, before any
// Init; m.db is set in Init but TopScores is only called after Init completes.
func (m *Module) Register(ctx *lifecycle.Context) error {
	registry.Provide(ctx.Registry, "leaderboard", m)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.db = ctx.DB
	m.log = ctx.Log

	// Persist a win per finished match. Async (event), so eventually consistent.
	bus.On(ctx.Bus, matchevents.FinishedEvent, func(r matchevents.Finished) {
		_, err := m.db.Exec(
			`INSERT INTO leaderboard.scores (player, wins) VALUES ($1, 1)
			 ON CONFLICT (player) DO UPDATE SET wins = leaderboard.scores.wins + 1`,
			r.Winner)
		if err != nil {
			m.log.Error("leaderboard upsert failed", "player", r.Winner, "err", err)
		}
	})

	registerOps(ctx, m)
	return nil
}

// TopScores implements leaderboardapi.Leaderboard: the top-ranked players
// (wins desc, player asc), capped at 100 — the same query and shape as the
// pre-migration handleList.
func (m *Module) TopScores(ctx context.Context) ([]leaderboardapi.Score, error) {
	rows, err := m.db.QueryContext(ctx,
		`SELECT player, wins FROM leaderboard.scores ORDER BY wins DESC, player ASC LIMIT 100`)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	out := []leaderboardapi.Score{}
	for rows.Next() {
		var s leaderboardapi.Score
		if err := rows.Scan(&s.Player, &s.Wins); err != nil {
			return nil, err
		}
		out = append(out, s)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return out, nil
}
