package inventory

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/modules/characters/charactersevents"
)

func testDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		t.Skipf("no postgres: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if err := db.PingContext(ctx); err != nil {
		_ = db.Close()
		t.Skipf("postgres unreachable: %v", err)
	}
	if _, err := db.Exec(schemaDDL); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	return db
}

func newUUID(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT gen_random_uuid()::text`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	return s
}

func qtyOf(holdings []Holding, itemID string) int {
	for _, h := range holdings {
		if h.ItemID == itemID {
			return h.Quantity
		}
	}
	return 0
}

func TestInventoryGrantStacks(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	s := &store{db: db, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	ctx := context.Background()

	owner := Owner{Type: "player", ID: newUUID(t, db)}
	if err := s.grant(ctx, owner, "coin", 5); err != nil {
		t.Fatal(err)
	}
	if err := s.grant(ctx, owner, "coin", 3); err != nil {
		t.Fatal(err)
	}
	list, err := s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if got := qtyOf(list, "coin"); got != 8 {
		t.Fatalf("coin quantity = %d, want 8 (grants should stack)", got)
	}
}

// TestInventoryReactsToCharacterLifecycle is the modularity payoff: inventory
// grants a starter item on character.created and wipes holdings on
// character.deleted, driven only by the event handlers — characters never calls
// inventory and there is no cross-module foreign key.
func TestInventoryReactsToCharacterLifecycle(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	s := &store{db: db, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	m := &Module{store: s, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	ctx := context.Background()

	charID := newUUID(t, db)
	owner := Owner{Type: "character", ID: charID}

	m.onCharacterCreated(charactersevents.Created{CharacterID: charID, Name: "Test", Class: "novice"})
	list, err := s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if qtyOf(list, starterItem) != 1 {
		t.Fatalf("after create: starter item not granted; holdings=%+v", list)
	}

	m.onCharacterDeleted(charactersevents.Deleted{CharacterID: charID})
	list, err = s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if len(list) != 0 {
		t.Fatalf("after delete: holdings not cleaned up; got %+v", list)
	}
}
