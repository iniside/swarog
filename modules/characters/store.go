package characters

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

// Character is a player-owned character. PlayerID is a plain reference to
// accounts.players — no cross-module foreign key (logical isolation).
type Character struct {
	ID        string    `json:"id"`
	PlayerID  string    `json:"player_id"`
	Name      string    `json:"name"`
	Class     string    `json:"class"`
	CreatedAt time.Time `json:"created_at"`
}

type store struct {
	db  *sql.DB
	log *slog.Logger
}

const cols = `id::text, player_id::text, name, class, created_at`

func (s *store) create(ctx context.Context, playerID, name, class string) (Character, error) {
	var c Character
	err := s.db.QueryRowContext(ctx,
		`INSERT INTO characters.characters (player_id, name, class)
		 VALUES ($1::uuid, $2, $3) RETURNING `+cols,
		playerID, name, class).
		Scan(&c.ID, &c.PlayerID, &c.Name, &c.Class, &c.CreatedAt)
	return c, err
}

func (s *store) listByPlayer(ctx context.Context, playerID string) ([]Character, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT `+cols+` FROM characters.characters WHERE player_id = $1::uuid ORDER BY created_at`, playerID)
	if err != nil {
		return nil, err
	}
	return scanCharacters(rows)
}

func (s *store) get(ctx context.Context, id string) (Character, bool, error) {
	var c Character
	err := s.db.QueryRowContext(ctx, `SELECT `+cols+` FROM characters.characters WHERE id = $1::uuid`, id).
		Scan(&c.ID, &c.PlayerID, &c.Name, &c.Class, &c.CreatedAt)
	if errors.Is(err, sql.ErrNoRows) || invalidUUID(err) {
		return Character{}, false, nil
	}
	return c, err == nil, err
}

// deleteOwned removes a character only if it belongs to playerID; returns whether
// a row was deleted.
func (s *store) deleteOwned(ctx context.Context, id, playerID string) (bool, error) {
	res, err := s.db.ExecContext(ctx,
		`DELETE FROM characters.characters WHERE id = $1::uuid AND player_id = $2::uuid`, id, playerID)
	if invalidUUID(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	n, _ := res.RowsAffected()
	return n > 0, nil
}

func (s *store) count(ctx context.Context) (int, error) {
	var n int
	err := s.db.QueryRowContext(ctx, `SELECT count(*) FROM characters.characters`).Scan(&n)
	return n, err
}

func (s *store) listAll(ctx context.Context, limit int) ([]Character, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT `+cols+` FROM characters.characters ORDER BY created_at DESC LIMIT $1`, limit)
	if err != nil {
		return nil, err
	}
	return scanCharacters(rows)
}

func scanCharacters(rows *sql.Rows) ([]Character, error) {
	defer rows.Close()
	out := []Character{}
	for rows.Next() {
		var c Character
		if err := rows.Scan(&c.ID, &c.PlayerID, &c.Name, &c.Class, &c.CreatedAt); err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

// invalidUUID reports a Postgres "invalid text representation" — i.e. a malformed
// id in the URL — so callers can treat it as not-found rather than a 500.
func invalidUUID(err error) bool {
	var pg *pgconn.PgError
	return errors.As(err, &pg) && pg.Code == "22P02"
}
