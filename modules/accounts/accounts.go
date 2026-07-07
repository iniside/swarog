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
	"gamebackend/modules/accounts/accountsadminrpc"
	"gamebackend/modules/accounts/accountsapi"
	"gamebackend/modules/accounts/accountsauthrpc"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

// Player and Identity are the accounts module's value types. Their canonical
// definitions live in accountsapi (the pure contract package) so the generated
// Auth glue can name them; the module aliases them so existing impl code
// (store methods, admin) refers to them unchanged.
type (
	Player   = accountsapi.Player
	Identity = accountsapi.Identity
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

// service is what other modules receive from Require("accounts"). It also backs
// both generated edge faces (Sessions + Auth) and the gateway's in-process
// operation invokers. store is set in Register (phase 1); bus/log/epic are wired in
// Init (before any request can arrive) — epic only when the epic provider is
// configured, so LoginEpic is contributed as an operation only in that case.
type service struct {
	store *store
	bus   *bus.Bus
	log   *slog.Logger
	epic  *oidcVerifier
}

// These assertions fail to compile if the service drifts from either generated
// contract — the single source of truth for the wire + gateway shapes.
var (
	_ accountsapi.Sessions = (*service)(nil)
	_ accountsapi.Auth     = (*service)(nil)
	// The admin fan-out capability is implemented by the Module (it wraps
	// adminSection, which reads m.store) — the source of truth for the edge glue.
	_ accountsapi.Admin = (*Module)(nil)
)

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.db = ctx.DB
	m.log = ctx.Log
	m.bus = ctx.Bus

	// Finish wiring the service Provided in Register: it needs the bus + logger to
	// mint sessions, emit PlayerRegistered, and (when configured below) the epic
	// verifier — the logic that used to live in the deleted HTTP handlers, now
	// behind the gateway-fronted operations.
	m.svc.bus = m.bus
	m.svc.log = m.log

	// dev/password provider — local testing convenience, gated off for prod. The
	// register/login OPERATIONS are contributed below only when this gate is ON,
	// mirroring the old conditional route registration.
	m.devAuth = envBool("ACCOUNTS_DEV_AUTH", true)
	if m.devAuth {
		ctx.Log.Warn("ACCOUNTS_DEV_AUTH is ON — /accounts/register and /accounts/login are enabled; turn OFF in production")
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
			m.svc.epic = v // service.LoginEpic verifies id_tokens through it
			ctx.Log.Info("epic provider enabled", "jwks", jwksURL, "aud", clientID)

			// Web OAuth (authorize-code) needs the confidential client secret. These
			// two routes stay HTTP-NATIVE (a browser redirect flow with an external
			// contract) — they are NOT operations and remain on ctx.Mux (plan Phase F).
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

	// Player operations: contribute each op's HTTP binding + in-process invoker so
	// the gateway fronts POST /accounts/register|login|login/epic and GET
	// /accounts/me, authenticates the AuthPlayer route (me) once, and dispatches to
	// m.svc. This REPLACES the deleted handleRegister/handleLogin/handleEpicLogin/
	// handleMe + their inline authedPlayer/bearerToken auth. register/login are
	// contributed only under devAuth; login/epic only when the epic provider is up.
	registerPlayerOps(ctx, m.svc, m.devAuth, m.epic != nil)

	// The accounts service was Provided in Register (phase 1); m.svc is set.
	// Split topology: expose Sessions (peer bearer verification) AND Auth (a front
	// gateway in a peer fronting the auth operations) over the shared QUIC edge
	// server. Registering handlers is pure wiring (no I/O); main() starts the
	// listener after all Inits.
	if m.Edge != nil {
		// Both edge faces are rpcgen-generated from the pure accountsapi contracts;
		// one RegisterServer per interface installs identity-aware adapters. Hand the
		// service to each glue AS the pure contract interface (not the concrete
		// *service): the glue depends on the capability, never the impl.
		var sess accountsapi.Sessions = m.svc
		accountsrpc.RegisterServer(m.Edge, sess)
		var auth accountsapi.Auth = m.svc
		accountsauthrpc.RegisterServer(m.Edge, auth)
		// adminData carries no identity — it is the admin fan-out, not a player op.
		var adminSvc accountsapi.Admin = m
		accountsadminrpc.RegisterServer(m.Edge, adminSvc)
		m.log.Info("edge handlers registered", "methods",
			[]string{accountsrpc.MethodVerifySession, accountsauthrpc.MethodRegister, accountsauthrpc.MethodLogin, accountsauthrpc.MethodLoginEpic, accountsauthrpc.MethodMe, accountsadminrpc.MethodAdminData})
	}

	// Appear in the admin portal (it renders whatever is contributed).
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: adminSectionName, Label: adminLabel, Render: m.adminSection})
	return nil
}

// Me returns the caller's own player + identities (player_id read from ctx,
// injected by the gateway after bearer verification — the AuthPlayer trust
// boundary; the service never reads a client-supplied identity). A missing
// identity is a StatusInvalid (→ 400); the gateway rejects an unauthenticated
// request with 401 before Me is ever called.
func (s *service) Me(ctx context.Context) (accountsapi.Player, []accountsapi.Identity, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return Player{}, nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	p, found, err := s.store.getPlayer(ctx, pid)
	if err != nil {
		s.log.Error("player lookup failed", "err", err)
		return Player{}, nil, err
	}
	if !found {
		return Player{}, nil, &opsapi.Error{Status: opsapi.StatusNotFound, Msg: "player not found"}
	}
	ids, err := s.store.identitiesOf(ctx, pid)
	if err != nil {
		s.log.Error("identities lookup failed", "err", err)
		return Player{}, nil, err
	}
	return p, ids, nil
}

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
