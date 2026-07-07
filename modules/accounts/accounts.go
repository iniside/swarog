package accounts

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"net/http"
	"os"
	"strings"
	"time"

	"gamebackend/bus"
	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts/accountsapi"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/registry"
)

// Module owns the "accounts" schema and the player-identity surface. It is a
// trusted verifier of external identities (epic) with a gated local password
// provider (dev) for testing. One product-scoped player_id, many providers.
type Module struct {
	db        *sql.DB
	log       *slog.Logger
	bus       *bus.Bus
	store     *store
	svc       *service
	devAuth   bool
	epic      *oidcVerifier
	epicOAuth *epicOAuth

	// Edge, when non-nil, is the process-wide QUIC RPC server (constructed and
	// started by main() only in a split that hosts this module). Init registers
	// the "accounts.verifySession" handler on it so a peer process can verify
	// session tokens over the wire. nil in the monolith — no edge exposure.
	Edge *edge.Server
}

func (*Module) Name() string       { return "accounts" }
func (*Module) Requires() []string { return nil }

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS accounts;

CREATE TABLE IF NOT EXISTS accounts.players (
	id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	display_name text        NOT NULL,
	created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS accounts.identities (
	provider    text NOT NULL,
	subject     text NOT NULL,
	player_id   uuid NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	secret_hash text,                         -- only dev/password uses it
	created_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (provider, subject)
);
CREATE INDEX IF NOT EXISTS identities_player_idx ON accounts.identities(player_id);

CREATE TABLE IF NOT EXISTS accounts.sessions (
	token      text PRIMARY KEY,
	player_id  uuid        NOT NULL REFERENCES accounts.players(id) ON DELETE CASCADE,
	created_at timestamptz NOT NULL DEFAULT now(),
	expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_player_idx ON accounts.sessions(player_id);`

func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Register constructs the store-backed service and offers it to other modules.
// It runs in Build's phase 1, before any Init, so a dependent's Require resolves
// regardless of registration order. It touches only ctx.DB (available now); the
// service closes over m.store alone, never a Required dependency.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.svc = &service{store: m.store}
	registry.Provide(ctx.Registry, "accounts", m.svc)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.db = ctx.DB
	m.log = ctx.Log
	m.bus = ctx.Bus

	// dev/password provider — local testing convenience, gated off for prod.
	m.devAuth = envBool("ACCOUNTS_DEV_AUTH", true)
	if m.devAuth {
		ctx.Log.Warn("ACCOUNTS_DEV_AUTH is ON — /accounts/register and /accounts/login are enabled; turn OFF in production")
		ctx.Mux.HandleFunc("POST /accounts/register", m.handleRegister)
		ctx.Mux.HandleFunc("POST /accounts/login", m.handleLogin)
	}

	// epic provider — the real federated path via Epic Account Services (OIDC).
	// Enabled only when configured. Defaults point at EAS endpoints (web OAuth);
	// sub is the Epic Account ID.
	if clientID := os.Getenv("EPIC_CLIENT_ID"); clientID != "" {
		jwksURL := envOr("EPIC_JWKS_URL", "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json")
		issuer := envOr("EPIC_ISSUER_PREFIX", "https://api.epicgames.dev/epic/oauth/v1")
		v, err := newOIDCVerifier(jwksURL, issuer, clientID)
		if err != nil {
			ctx.Log.Error("epic provider disabled: jwks unavailable", "err", err)
		} else {
			m.epic = v
			ctx.Mux.HandleFunc("POST /accounts/login/epic", m.handleEpicLogin)
			ctx.Log.Info("epic provider enabled", "jwks", jwksURL, "aud", clientID)

			// Web OAuth (authorize-code) needs the confidential client secret.
			if secret := os.Getenv("EPIC_CLIENT_SECRET"); secret != "" {
				m.epicOAuth = &epicOAuth{
					clientID:     clientID,
					clientSecret: secret,
					redirectURI:  envOr("EPIC_REDIRECT_URI", "http://localhost:8080/accounts/epic/callback"),
					authorizeURL: envOr("EPIC_AUTHORIZE_URL", "https://www.epicgames.com/id/authorize"),
					tokenURL:     envOr("EPIC_TOKEN_URL", "https://api.epicgames.dev/epic/oauth/v1/token"),
					verifier:     v,
					httpc:        &http.Client{Timeout: 10 * time.Second},
					states:       map[string]oauthState{},
				}
				ctx.Mux.HandleFunc("POST /accounts/epic/start", m.handleEpicStart)
				ctx.Mux.HandleFunc("GET /accounts/epic/callback", m.handleEpicCallback)
				ctx.Log.Info("epic OAuth enabled", "redirect", m.epicOAuth.redirectURI)
			}
		}
	}

	ctx.Mux.HandleFunc("GET /accounts/me", m.handleMe)

	// The accounts service was Provided in Register (phase 1); m.svc is set.
	// Split topology: expose VerifySession over the shared QUIC edge server so a
	// peer process can authenticate bearer tokens. Registering a handler is pure
	// wiring (no I/O); main() starts the listener after all Inits.
	if m.Edge != nil {
		// The VerifySession edge glue (envelope + adapter) is rpcgen-generated
		// from accountsapi.Sessions — one RegisterServer call replaces the hand
		// adapter + mirrored DTOs + wire_contract_test that used to live here.
		// Hand the service to the glue AS the pure contract interface (not the
		// concrete *service): the glue depends on the capability, never the impl.
		var sess accountsapi.Sessions = m.svc
		accountsrpc.RegisterServer(m.Edge, sess)
		m.log.Info("edge handler registered", "method", accountsrpc.MethodVerifySession)
	}

	// Appear in the admin portal (it renders whatever is contributed).
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: adminSectionName, Label: adminLabel, Render: m.adminSection})
	// GET /admin-data/accounts: the same content over HTTP for a remote admin.
	ctx.Mux.HandleFunc("GET /admin-data/"+adminItemID, m.handleAdminData)
	return nil
}

func (m *Module) handleMe(w http.ResponseWriter, r *http.Request) {
	p, ok := m.authedPlayer(r)
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	ids, err := m.store.identitiesOf(r.Context(), p.ID)
	if err != nil {
		m.log.Error("identities lookup failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, meResponse{Player: p, Identities: ids})
}

type meResponse struct {
	Player
	Identities []Identity `json:"identities"`
}

type authResponse struct {
	PlayerID string `json:"player_id"`
	Token    string `json:"token"`
}

func (m *Module) issueSession(w http.ResponseWriter, r *http.Request, p Player, status int) {
	token, err := m.store.newSession(r.Context(), p.ID)
	if err != nil {
		m.log.Error("session create failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, status, authResponse{PlayerID: p.ID, Token: token})
}

func (m *Module) authedPlayer(r *http.Request) (Player, bool) {
	token := bearerToken(r)
	if token == "" {
		return Player{}, false
	}
	p, ok, err := m.store.playerBySession(r.Context(), token)
	if err != nil {
		m.log.Error("session lookup failed", "err", err)
		return Player{}, false
	}
	return p, ok
}

// service is what other modules receive from Require("accounts").
type service struct{ store *store }

// service is the impl behind the generated VerifySession edge glue: it satisfies
// the pure accountsapi.Sessions contract rpcgen reads. This assertion fails to
// compile if the service's VerifySession drifts from the generated server adapter.
var _ accountsapi.Sessions = (*service)(nil)

// VerifySession resolves a bearer token to its player. An unknown/expired token
// is ("", false, nil); a store failure now propagates as a non-nil error (B2)
// instead of masquerading as an invalid session, so a consumer can answer 503
// on infrastructure failure rather than 401.
func (s *service) VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error) {
	p, ok, err := s.store.playerBySession(ctx, token)
	if err != nil {
		return "", false, err
	}
	if !ok {
		return "", false, nil
	}
	return p.ID, true, nil
}

func (s *service) GetPlayer(ctx context.Context, id string) (Player, bool) {
	p, ok, err := s.store.getPlayer(ctx, id)
	if err != nil {
		return Player{}, false
	}
	return p, ok
}

func bearerToken(r *http.Request) string {
	if after, found := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); found {
		return after
	}
	return ""
}

func decodeJSON(w http.ResponseWriter, r *http.Request, dst any) bool {
	if err := json.NewDecoder(r.Body).Decode(dst); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func envBool(key string, def bool) bool {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	return v == "1" || strings.EqualFold(v, "true") || strings.EqualFold(v, "on")
}

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
