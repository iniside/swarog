package accounts

import (
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"

	"golang.org/x/crypto/argon2"

	"gamebackend/bus"
	"gamebackend/modules/accounts/accountsapi"
	"gamebackend/modules/accounts/accountsevents"
	"gamebackend/opsapi"
)

// argon2id parameters (OWASP-ish defaults).
const (
	argonTime    = 1
	argonMemory  = 64 * 1024 // KiB
	argonThreads = 4
	argonKeyLen  = 32
	argonSaltLen = 16
)

func hashPassword(pw string) (string, error) {
	salt := make([]byte, argonSaltLen)
	if _, err := rand.Read(salt); err != nil {
		return "", err
	}
	key := argon2.IDKey([]byte(pw), salt, argonTime, argonMemory, argonThreads, argonKeyLen)
	return fmt.Sprintf("$argon2id$v=%d$m=%d,t=%d,p=%d$%s$%s",
		argon2.Version, argonMemory, argonTime, argonThreads,
		base64.RawStdEncoding.EncodeToString(salt),
		base64.RawStdEncoding.EncodeToString(key)), nil
}

func verifyPassword(encoded, pw string) bool {
	parts := strings.Split(encoded, "$")
	if len(parts) != 6 || parts[1] != "argon2id" {
		return false
	}
	var memory, time uint32
	var threads uint8
	if _, err := fmt.Sscanf(parts[3], "m=%d,t=%d,p=%d", &memory, &time, &threads); err != nil {
		return false
	}
	salt, err := base64.RawStdEncoding.DecodeString(parts[4])
	if err != nil {
		return false
	}
	want, err := base64.RawStdEncoding.DecodeString(parts[5])
	if err != nil {
		return false
	}
	got := argon2.IDKey([]byte(pw), salt, time, memory, threads, uint32(len(want)))
	return subtle.ConstantTimeCompare(want, got) == 1
}

// Register is the dev/password self-registration operation (AuthNone): it creates
// a player + dev identity, emits PlayerRegistered, and mints a session. A missing
// email/password is StatusInvalid (→ 400); a duplicate email is StatusConflict
// (→ 409) — the same 400/409 the deleted handleRegister returned, now typed.
func (s *service) Register(ctx context.Context, email, password, displayName string) (accountsapi.Session, error) {
	if email == "" || password == "" {
		return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "email and password are required"}
	}
	display := displayName
	if display == "" {
		display = email
	}

	hash, err := hashPassword(password)
	if err != nil {
		s.log.Error("register: hash failed", "err", err)
		return accountsapi.Session{}, err
	}
	p, err := s.store.registerPassword(ctx, email, hash, display)
	if errors.Is(err, ErrEmailTaken) {
		return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusConflict, Msg: "email already registered"}
	}
	if err != nil {
		s.log.Error("register failed", "err", err)
		return accountsapi.Session{}, err
	}

	bus.Emit(s.bus, accountsevents.PlayerRegisteredEvent, accountsevents.PlayerRegistered{
		PlayerID: p.ID, DisplayName: p.DisplayName, Provider: "dev",
	})
	return s.issueSession(ctx, p)
}

// Login is the dev/password login operation (AuthNone). Bad credentials — an
// unknown email or a wrong password, deliberately indistinguishable so the
// endpoint does not leak which emails exist — are StatusUnauthorized (→ 401),
// exactly the 401 the deleted handleLogin returned.
func (s *service) Login(ctx context.Context, email, password string) (accountsapi.Session, error) {
	p, hash, err := s.store.passwordIdentity(ctx, email)
	if err != nil {
		if errors.Is(err, ErrInvalidCredentials) {
			return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusUnauthorized, Msg: "invalid credentials"}
		}
		s.log.Error("login failed", "err", err)
		return accountsapi.Session{}, err
	}
	if !verifyPassword(hash, password) {
		return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusUnauthorized, Msg: "invalid credentials"}
	}
	return s.issueSession(ctx, p)
}

// issueSession mints a fresh bearer token for p and returns it as a Session (the
// {player_id, token} the gateway JSON-encodes). A session-store failure propagates
// as a plain error → StatusInternal (→ 500).
func (s *service) issueSession(ctx context.Context, p Player) (accountsapi.Session, error) {
	token, err := s.store.newSession(ctx, p.ID)
	if err != nil {
		s.log.Error("session create failed", "err", err)
		return accountsapi.Session{}, err
	}
	return accountsapi.Session{PlayerID: p.ID, Token: token}, nil
}
