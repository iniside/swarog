package inventory

import (
	"context"
	"database/sql"
	"log/slog"

	"gamebackend/modules/inventory/inventoryapi"
)

// Owner is who an inventory belongs to. Type is "player" or "character"; ID is
// the player or character uuid. The polymorphism lives entirely inside this
// module — owners are referenced by id, with no cross-module foreign key.
type Owner struct {
	Type string
	ID   string
}

// Holding is aliased from the pure inventoryapi contract so the impl and the
// generated glue name the SAME type (the response shape of the player
// operations), the way characters aliases charactersapi.Character.
type Holding = inventoryapi.Holding

type store struct {
	db  *sql.DB
	log *slog.Logger
}

// rowQuerier is the subset of *sql.DB / *sql.Tx the event-effect writes use, so
// the same grant/clear logic runs either against the pool (the best-effort bus
// path) OR inside the sink's transaction (atomic with the inbox dedup row).
type rowQuerier interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
}

func (s *store) grant(ctx context.Context, owner Owner, itemID string, qty int) error {
	return s.grantExec(ctx, s.db, owner, itemID, qty)
}

func (s *store) grantExec(ctx context.Context, q rowQuerier, owner Owner, itemID string, qty int) error {
	_, err := q.ExecContext(ctx,
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

// clearOwnerExec removes every holding of an owner — the event-driven cleanup
// when a character (or later a player) is deleted. Runs against the pool (bus
// path) or the sink's tx.
func (s *store) clearOwnerExec(ctx context.Context, q rowQuerier, owner Owner) (int64, error) {
	res, err := q.ExecContext(ctx,
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
	defer func() { _ = rows.Close() }()
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
