package characters

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"
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
		db.Close()
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

func TestCharactersStore(t *testing.T) {
	db := testDB(t)
	defer db.Close()
	s := &store{db: db, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	ctx := context.Background()

	pid := newUUID(t, db)
	other := newUUID(t, db)

	c, err := s.create(ctx, pid, "Aria", "mage")
	if err != nil || c.ID == "" || c.PlayerID != pid || c.Class != "mage" {
		t.Fatalf("create: %+v err=%v", c, err)
	}

	list, err := s.listByPlayer(ctx, pid)
	if err != nil || len(list) != 1 || list[0].ID != c.ID {
		t.Fatalf("listByPlayer: %+v err=%v", list, err)
	}

	got, ok, err := s.get(ctx, c.ID)
	if err != nil || !ok || got.PlayerID != pid {
		t.Fatalf("get: ok=%v player=%q err=%v", ok, got.PlayerID, err)
	}

	// a malformed id is not-found, not an error
	if _, ok, err := s.get(ctx, "not-a-uuid"); ok || err != nil {
		t.Fatalf("get(bad id): ok=%v err=%v", ok, err)
	}

	// not deleted when owned by someone else
	if deleted, err := s.deleteOwned(ctx, c.ID, other); err != nil || deleted {
		t.Fatalf("delete by non-owner: deleted=%v err=%v", deleted, err)
	}
	// deleted by the owner
	if deleted, err := s.deleteOwned(ctx, c.ID, pid); err != nil || !deleted {
		t.Fatalf("delete by owner: deleted=%v err=%v", deleted, err)
	}
	if _, ok, _ := s.get(ctx, c.ID); ok {
		t.Fatal("character still present after delete")
	}
}
