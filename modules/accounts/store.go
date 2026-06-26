package accounts

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/base64"
	"errors"
	"log/slog"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

const sessionTTL = 30 * 24 * time.Hour

var (
	ErrEmailTaken         = errors.New("email already registered")
	ErrInvalidCredentials = errors.New("invalid credentials")
	ErrIdentityLinked     = errors.New("identity already linked")
)

// Player is our product-scoped identity (the EOS PUID analogue).
type Player struct {
	ID          string `json:"player_id"`
	DisplayName string `json:"display_name"`
}

// Identity is one credential mapping (provider, subject) -> player.
type Identity struct {
	Provider string `json:"provider"`
	Subject  string `json:"subject"`
}

// store is pure persistence for the accounts schema. No event/bus knowledge —
// the module emits PlayerRegistered based on the bool these methods return.
type store struct {
	db  *sql.DB
	log *slog.Logger
}

// registerPassword provisions a new player with a dev/password identity.
func (s *store) registerPassword(ctx context.Context, email, passwordHash, displayName string) (Player, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return Player{}, err
	}
	defer tx.Rollback()

	p, err := insertPlayerWithIdentity(ctx, tx, "dev", email, displayName,
		sql.NullString{String: passwordHash, Valid: true})
	if err != nil {
		if isUniqueViolation(err) {
			return Player{}, ErrEmailTaken
		}
		return Player{}, err
	}
	if err := tx.Commit(); err != nil {
		return Player{}, err
	}
	return p, nil
}

// passwordIdentity returns the player and stored hash for a dev identity, or
// ErrInvalidCredentials if there is none (same error as a bad password, so the
// endpoint doesn't leak which emails exist).
func (s *store) passwordIdentity(ctx context.Context, email string) (Player, string, error) {
	var p Player
	var hash sql.NullString
	err := s.db.QueryRowContext(ctx,
		`SELECT p.id::text, p.display_name, i.secret_hash
		   FROM accounts.identities i
		   JOIN accounts.players p ON p.id = i.player_id
		  WHERE i.provider = 'dev' AND i.subject = $1`, email).
		Scan(&p.ID, &p.DisplayName, &hash)
	switch {
	case errors.Is(err, sql.ErrNoRows) || (err == nil && !hash.Valid):
		return Player{}, "", ErrInvalidCredentials
	case err != nil:
		return Player{}, "", err
	}
	return p, hash.String, nil
}

// findOrCreateExternal maps a verified external identity to a player, creating
// one on first sight (implicit registration, like EOS first-login). The bool is
// true when a new player was provisioned.
func (s *store) findOrCreateExternal(ctx context.Context, provider, subject, displayName string) (Player, bool, error) {
	if p, ok, err := s.playerByIdentity(ctx, provider, subject); err != nil || ok {
		return p, false, err
	}

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return Player{}, false, err
	}
	defer tx.Rollback()

	p, err := insertPlayerWithIdentity(ctx, tx, provider, subject, displayName, sql.NullString{})
	if err != nil {
		if isUniqueViolation(err) { // raced with a concurrent first-login
			p2, ok2, e2 := s.playerByIdentity(ctx, provider, subject)
			if e2 == nil && ok2 {
				return p2, false, nil
			}
		}
		return Player{}, false, err
	}
	if err := tx.Commit(); err != nil {
		return Player{}, false, err
	}
	return p, true, nil
}

// linkIdentity attaches an already-verified external identity to an existing
// player. Returns ErrIdentityLinked if that (provider, subject) is taken.
func (s *store) linkIdentity(ctx context.Context, playerID, provider, subject string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO accounts.identities (provider, subject, player_id) VALUES ($1, $2, $3::uuid)`,
		provider, subject, playerID)
	if isUniqueViolation(err) {
		return ErrIdentityLinked
	}
	return err
}

func (s *store) playerByIdentity(ctx context.Context, provider, subject string) (Player, bool, error) {
	var p Player
	err := s.db.QueryRowContext(ctx,
		`SELECT p.id::text, p.display_name
		   FROM accounts.identities i
		   JOIN accounts.players p ON p.id = i.player_id
		  WHERE i.provider = $1 AND i.subject = $2`, provider, subject).
		Scan(&p.ID, &p.DisplayName)
	if errors.Is(err, sql.ErrNoRows) {
		return Player{}, false, nil
	}
	return p, err == nil, err
}

func (s *store) newSession(ctx context.Context, playerID string) (string, error) {
	token, err := newToken()
	if err != nil {
		return "", err
	}
	_, err = s.db.ExecContext(ctx,
		`INSERT INTO accounts.sessions (token, player_id, expires_at) VALUES ($1, $2::uuid, $3)`,
		token, playerID, time.Now().Add(sessionTTL))
	if err != nil {
		return "", err
	}
	return token, nil
}

// playerBySession resolves a bearer token to its player, ignoring expired ones.
func (s *store) playerBySession(ctx context.Context, token string) (Player, bool, error) {
	var p Player
	err := s.db.QueryRowContext(ctx,
		`SELECT p.id::text, p.display_name
		   FROM accounts.sessions s
		   JOIN accounts.players p ON p.id = s.player_id
		  WHERE s.token = $1 AND s.expires_at > now()`, token).
		Scan(&p.ID, &p.DisplayName)
	if errors.Is(err, sql.ErrNoRows) {
		return Player{}, false, nil
	}
	return p, err == nil, err
}

func (s *store) getPlayer(ctx context.Context, id string) (Player, bool, error) {
	var p Player
	err := s.db.QueryRowContext(ctx,
		`SELECT id::text, display_name FROM accounts.players WHERE id = $1::uuid`, id).
		Scan(&p.ID, &p.DisplayName)
	if errors.Is(err, sql.ErrNoRows) {
		return Player{}, false, nil
	}
	return p, err == nil, err
}

func (s *store) identitiesOf(ctx context.Context, playerID string) ([]Identity, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT provider, subject FROM accounts.identities
		  WHERE player_id = $1::uuid ORDER BY provider, subject`, playerID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	out := []Identity{}
	for rows.Next() {
		var id Identity
		if err := rows.Scan(&id.Provider, &id.Subject); err != nil {
			return nil, err
		}
		out = append(out, id)
	}
	return out, rows.Err()
}

// insertPlayerWithIdentity creates a player and its first identity inside a tx,
// so a failed identity insert rolls back the orphaned player.
func insertPlayerWithIdentity(ctx context.Context, tx *sql.Tx, provider, subject, displayName string, secret sql.NullString) (Player, error) {
	var p Player
	if err := tx.QueryRowContext(ctx,
		`INSERT INTO accounts.players (display_name) VALUES ($1) RETURNING id::text, display_name`,
		displayName).Scan(&p.ID, &p.DisplayName); err != nil {
		return Player{}, err
	}
	_, err := tx.ExecContext(ctx,
		`INSERT INTO accounts.identities (provider, subject, player_id, secret_hash)
		 VALUES ($1, $2, $3::uuid, $4)`, provider, subject, p.ID, secret)
	if err != nil {
		return Player{}, err
	}
	return p, nil
}

func isUniqueViolation(err error) bool {
	var pg *pgconn.PgError
	return errors.As(err, &pg) && pg.Code == "23505"
}

func newToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}
