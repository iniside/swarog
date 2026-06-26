package inventory

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"
)

var ErrUnknownItem = errors.New("unknown item")

// Owner is who an inventory belongs to. Type is "player" or "character"; ID is
// the player or character uuid. The polymorphism lives entirely inside this
// module — owners are referenced by id, with no cross-module foreign key.
type Owner struct {
	Type string
	ID   string
}

type Holding struct {
	OwnerType string `json:"owner_type"`
	OwnerID   string `json:"owner_id"`
	ItemID    string `json:"item_id"`
	ItemName  string `json:"item_name"`
	Quantity  int    `json:"quantity"`
}

type store struct {
	db  *sql.DB
	log *slog.Logger
}

func (s *store) grant(ctx context.Context, owner Owner, itemID string, qty int) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO inventory.holdings (owner_type, owner_id, item_id, quantity)
		 VALUES ($1, $2::uuid, $3, $4)
		 ON CONFLICT (owner_type, owner_id, item_id)
		 DO UPDATE SET quantity = inventory.holdings.quantity + EXCLUDED.quantity`,
		owner.Type, owner.ID, itemID, qty)
	return err
}

func (s *store) list(ctx context.Context, owner Owner) ([]Holding, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT h.owner_type, h.owner_id::text, h.item_id, i.name, h.quantity
		   FROM inventory.holdings h
		   JOIN inventory.items i ON i.id = h.item_id
		  WHERE h.owner_type = $1 AND h.owner_id = $2::uuid
		  ORDER BY h.item_id`, owner.Type, owner.ID)
	if err != nil {
		return nil, err
	}
	return scanHoldings(rows)
}

// clearOwner removes every holding of an owner — the event-driven cleanup when a
// character (or later a player) is deleted.
func (s *store) clearOwner(ctx context.Context, owner Owner) (int64, error) {
	res, err := s.db.ExecContext(ctx,
		`DELETE FROM inventory.holdings WHERE owner_type = $1 AND owner_id = $2::uuid`,
		owner.Type, owner.ID)
	if err != nil {
		return 0, err
	}
	n, _ := res.RowsAffected()
	return n, nil
}

func (s *store) itemExists(ctx context.Context, itemID string) (bool, error) {
	var ok bool
	err := s.db.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM inventory.items WHERE id = $1)`, itemID).Scan(&ok)
	return ok, err
}

func (s *store) stats(ctx context.Context) (holdings, owners int, err error) {
	err = s.db.QueryRowContext(ctx,
		`SELECT (SELECT count(*) FROM inventory.holdings),
		        (SELECT count(*) FROM (SELECT DISTINCT owner_type, owner_id FROM inventory.holdings) t)`).
		Scan(&holdings, &owners)
	return
}

func (s *store) listAll(ctx context.Context, limit int) ([]Holding, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT h.owner_type, h.owner_id::text, h.item_id, i.name, h.quantity
		   FROM inventory.holdings h
		   JOIN inventory.items i ON i.id = h.item_id
		  ORDER BY h.owner_type, h.owner_id
		  LIMIT $1`, limit)
	if err != nil {
		return nil, err
	}
	return scanHoldings(rows)
}

func scanHoldings(rows *sql.Rows) ([]Holding, error) {
	defer rows.Close()
	out := []Holding{}
	for rows.Next() {
		var h Holding
		if err := rows.Scan(&h.OwnerType, &h.OwnerID, &h.ItemID, &h.ItemName, &h.Quantity); err != nil {
			return nil, err
		}
		out = append(out, h)
	}
	return out, rows.Err()
}
