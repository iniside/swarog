package characters

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"

	"github.com/jackc/pgx/v5/pgconn"

	"gamebackend/modules/characters/charactersapi"
)

// Character is a player-owned character. PlayerID is a plain reference to
// accounts.players — no cross-module foreign key (logical isolation). It is an
// alias of charactersapi.Character (the shape the Player capability returns), so
// the impl and the generated glue name the exact same type.
type Character = charactersapi.Character

type store struct {
	db  *sql.DB
	log *slog.Logger
}

// rowQuerier is the subset of *sql.DB / *sql.Tx the write paths use, so the same
// method can run either directly against the pool OR inside a transaction (the
// create+outbox and delete+outbox writes commit atomically — a *sql.Tx is passed).
type rowQuerier interface {
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
}

const cols = `id::text, player_id::text, name, class, created_at`

func (s *store) create(ctx context.Context, playerID, name, class string) (Character, error) {
	return s.createTx(ctx, s.db, playerID, name, class)
}

// createTx inserts a character using the given querier (the pool or a tx). The
// handler passes a tx so the character row and its outbox row commit together.
func (s *store) createTx(ctx context.Context, q rowQuerier, playerID, name, class string) (Character, error) {
	var c Character
	err := q.QueryRowContext(ctx,
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
	return s.deleteOwnedTx(ctx, s.db, id, playerID)
}

// deleteOwnedTx is deleteOwned against the given querier (the pool or a tx), so
// the delete and its outbox row commit atomically.
func (s *store) deleteOwnedTx(ctx context.Context, q rowQuerier, id, playerID string) (bool, error) {
	res, err := q.ExecContext(ctx,
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
	defer func() { _ = rows.Close() }()
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
