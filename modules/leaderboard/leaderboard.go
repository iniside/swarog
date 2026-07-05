package leaderboard

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"net/http"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/match/matchevents"
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

	ctx.Mux.HandleFunc("GET /leaderboard", m.handleList)
	return nil
}

func (m *Module) handleList(w http.ResponseWriter, req *http.Request) {
	rows, err := m.db.QueryContext(req.Context(),
		`SELECT player, wins FROM leaderboard.scores ORDER BY wins DESC, player ASC LIMIT 100`)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	defer rows.Close()

	type row struct {
		Player string `json:"player"`
		Wins   int64  `json:"wins"`
	}
	out := []row{}
	for rows.Next() {
		var r row
		if err := rows.Scan(&r.Player, &r.Wins); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		out = append(out, r)
	}
	if err := rows.Err(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(out)
}
