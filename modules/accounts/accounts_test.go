package accounts

import (
	"context"
	"crypto/rand"
	"crypto/rsa"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/core"
)

func testLogger() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

func suffix(t *testing.T) string {
	t.Helper()
	s, err := newToken()
	if err != nil {
		t.Fatal(err)
	}
	return s[:12]
}

// --- unit: argon2id ---

func TestArgon2Roundtrip(t *testing.T) {
	h, err := hashPassword("hunter2")
	if err != nil {
		t.Fatal(err)
	}
	if !verifyPassword(h, "hunter2") {
		t.Fatal("correct password rejected")
	}
	if verifyPassword(h, "wrong") {
		t.Fatal("wrong password accepted")
	}
	if verifyPassword("not-a-hash", "hunter2") {
		t.Fatal("garbage hash accepted")
	}
}

// --- unit: OIDC verifier against a local JWKS (proves the Epic federation logic
// with no dependency on Epic) ---

func TestOIDCVerifier(t *testing.T) {
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	const kid = "test-key"
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(buildJWKS(kid, &key.PublicKey))
	}))
	defer srv.Close()

	v, err := newOIDCVerifier(srv.URL, "https://api.epicgames.dev", "client-123")
	if err != nil {
		t.Fatal(err)
	}

	sign := func(claims jwt.MapClaims) string {
		tok := jwt.NewWithClaims(jwt.SigningMethodRS256, claims)
		tok.Header["kid"] = kid
		s, err := tok.SignedString(key)
		if err != nil {
			t.Fatal(err)
		}
		return s
	}
	future := time.Now().Add(time.Hour).Unix()

	good := sign(jwt.MapClaims{"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "PUID-1", "exp": future})
	sub, err := v.verify(good)
	if err != nil || sub != "PUID-1" {
		t.Fatalf("valid token: sub=%q err=%v", sub, err)
	}

	bad := map[string]jwt.MapClaims{
		"wrong aud":   {"iss": "https://api.epicgames.dev/x", "aud": "other", "sub": "s", "exp": future},
		"expired":     {"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "s", "exp": time.Now().Add(-time.Hour).Unix()},
		"bad issuer":  {"iss": "https://evil.example/x", "aud": "client-123", "sub": "s", "exp": future},
		"missing sub": {"iss": "https://api.epicgames.dev/x", "aud": "client-123", "exp": future},
	}
	for name, claims := range bad {
		if _, err := v.verify(sign(claims)); err == nil {
			t.Errorf("%s: token accepted, want rejected", name)
		}
	}

	// alg=none must be refused.
	noneTok := jwt.NewWithClaims(jwt.SigningMethodNone, jwt.MapClaims{"iss": "https://api.epicgames.dev/x", "aud": "client-123", "sub": "s", "exp": future})
	noneStr, err := noneTok.SignedString(jwt.UnsafeAllowNoneSignatureType)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := v.verify(noneStr); err == nil {
		t.Error("alg=none accepted, want rejected")
	}
}

func buildJWKS(kid string, pub *rsa.PublicKey) []byte {
	jwk := map[string]any{
		"keys": []map[string]string{{
			"kty": "RSA",
			"kid": kid,
			"use": "sig",
			"alg": "RS256",
			"n":   base64.RawURLEncoding.EncodeToString(pub.N.Bytes()),
			"e":   base64.RawURLEncoding.EncodeToString(big.NewInt(int64(pub.E)).Bytes()),
		}},
	}
	b, _ := json.Marshal(jwk)
	return b
}

// --- integration: live Postgres (skips if unreachable) ---

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

func TestStoreRegisterLoginSession(t *testing.T) {
	db := testDB(t)
	defer db.Close()
	s := &store{db: db, log: testLogger()}
	ctx := context.Background()

	email := "u-" + suffix(t) + "@test.local"
	hash, _ := hashPassword("secret")

	p, err := s.registerPassword(ctx, email, hash, "Tester")
	if err != nil {
		t.Fatalf("register: %v", err)
	}
	if p.ID == "" || p.DisplayName != "Tester" {
		t.Fatalf("bad player: %+v", p)
	}

	if _, err := s.registerPassword(ctx, email, hash, "Tester"); !errors.Is(err, ErrEmailTaken) {
		t.Fatalf("duplicate register: want ErrEmailTaken, got %v", err)
	}

	got, storedHash, err := s.passwordIdentity(ctx, email)
	if err != nil || got.ID != p.ID {
		t.Fatalf("passwordIdentity: id=%q err=%v", got.ID, err)
	}
	if !verifyPassword(storedHash, "secret") || verifyPassword(storedHash, "nope") {
		t.Fatal("stored hash does not verify correctly")
	}
	if _, _, err := s.passwordIdentity(ctx, "ghost-"+suffix(t)+"@test.local"); !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("unknown email: want ErrInvalidCredentials, got %v", err)
	}

	token, err := s.newSession(ctx, p.ID)
	if err != nil {
		t.Fatalf("newSession: %v", err)
	}
	sp, ok, err := s.playerBySession(ctx, token)
	if err != nil || !ok || sp.ID != p.ID {
		t.Fatalf("playerBySession: ok=%v id=%q err=%v", ok, sp.ID, err)
	}
	if _, ok, _ := s.playerBySession(ctx, "garbage-token"); ok {
		t.Fatal("garbage token resolved to a player")
	}
}

func TestStoreFindOrCreateExternal(t *testing.T) {
	db := testDB(t)
	defer db.Close()
	s := &store{db: db, log: testLogger()}
	ctx := context.Background()

	sub := "puid-" + suffix(t)
	p, created, err := s.findOrCreateExternal(ctx, "epic", sub, "epic:new")
	if err != nil || !created {
		t.Fatalf("first login: created=%v err=%v", created, err)
	}
	again, created2, err := s.findOrCreateExternal(ctx, "epic", sub, "epic:new")
	if err != nil || created2 {
		t.Fatalf("second login: created=%v err=%v", created2, err)
	}
	if again.ID != p.ID {
		t.Fatalf("same identity mapped to different players: %q vs %q", again.ID, p.ID)
	}
}

// TestEpicOAuthLinkFlow drives the whole link flow against a mock Epic (local
// JWKS + token endpoint), proving the callback exchanges the code, verifies the
// id_token and links the Epic identity to the session's player — no real Epic.
func TestEpicOAuthLinkFlow(t *testing.T) {
	db := testDB(t)
	defer db.Close()
	s := &store{db: db, log: testLogger()}
	ctx := context.Background()

	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	const kid, clientID, issuer = "k1", "client-xyz", "https://eas.example"
	epicAcct := "epicacct-" + suffix(t)

	jwks := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Write(buildJWKS(kid, &key.PublicKey))
	}))
	defer jwks.Close()

	tok := jwt.NewWithClaims(jwt.SigningMethodRS256, jwt.MapClaims{
		"iss": issuer + "/x", "aud": clientID, "sub": epicAcct, "exp": time.Now().Add(time.Hour).Unix(),
	})
	tok.Header["kid"] = kid
	idToken, err := tok.SignedString(key)
	if err != nil {
		t.Fatal(err)
	}
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]string{"id_token": idToken})
	}))
	defer tokenSrv.Close()

	v, err := newOIDCVerifier(jwks.URL, issuer, clientID)
	if err != nil {
		t.Fatal(err)
	}
	o := &epicOAuth{
		clientID: clientID, clientSecret: "secret", redirectURI: "http://localhost/cb",
		authorizeURL: "http://localhost/authorize", tokenURL: tokenSrv.URL,
		verifier: v, httpc: tokenSrv.Client(), states: map[string]oauthState{},
	}
	m := &Module{store: s, log: testLogger(), bus: core.NewBus(testLogger()), epic: v, epicOAuth: o}

	// a logged-in dev player to link onto
	hash, _ := hashPassword("pw")
	p, err := s.registerPassword(ctx, "link-"+suffix(t)+"@test.local", hash, "Linker")
	if err != nil {
		t.Fatal(err)
	}
	sess, err := s.newSession(ctx, p.ID)
	if err != nil {
		t.Fatal(err)
	}
	state, err := o.newState(sess) // link flow bound to that session
	if err != nil {
		t.Fatal(err)
	}

	req := httptest.NewRequest(http.MethodGet, "/accounts/epic/callback?code=abc&state="+state, nil)
	rec := httptest.NewRecorder()
	m.handleEpicCallback(rec, req)

	if rec.Code != http.StatusSeeOther || rec.Header().Get("Location") != "/?epic=linked" {
		t.Fatalf("callback: code=%d loc=%q body=%s", rec.Code, rec.Header().Get("Location"), rec.Body.String())
	}
	ids, err := s.identitiesOf(ctx, p.ID)
	if err != nil {
		t.Fatal(err)
	}
	linked := false
	for _, id := range ids {
		if id.Provider == "epic" && id.Subject == epicAcct {
			linked = true
		}
	}
	if !linked {
		t.Fatalf("epic identity not linked to player; got %+v", ids)
	}
}
