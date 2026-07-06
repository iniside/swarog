package accounts

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"golang.org/x/crypto/argon2"

	"gamebackend/bus"
	"gamebackend/modules/accounts/accountsevents"
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

// handleRegister: dev-only self-registration. Creates a player + dev identity.
func (m *Module) handleRegister(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Email       string `json:"email"`
		Password    string `json:"password"`
		DisplayName string `json:"displayName"`
	}
	if !decodeJSON(w, r, &in) {
		return
	}
	if in.Email == "" || in.Password == "" {
		http.Error(w, "email and password are required", http.StatusBadRequest)
		return
	}
	display := in.DisplayName
	if display == "" {
		display = in.Email
	}

	hash, err := hashPassword(in.Password)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	p, err := m.store.registerPassword(r.Context(), in.Email, hash, display)
	if errors.Is(err, ErrEmailTaken) {
		http.Error(w, "email already registered", http.StatusConflict)
		return
	}
	if err != nil {
		m.log.Error("register failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	bus.Emit(m.bus, accountsevents.PlayerRegisteredEvent, accountsevents.PlayerRegistered{
		PlayerID: p.ID, DisplayName: p.DisplayName, Provider: "dev",
	})
	m.issueSession(w, r, p, http.StatusCreated)
}

// handleLogin: dev-only password login.
func (m *Module) handleLogin(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Email    string `json:"email"`
		Password string `json:"password"`
	}
	if !decodeJSON(w, r, &in) {
		return
	}
	p, hash, err := m.store.passwordIdentity(r.Context(), in.Email)
	if err != nil {
		if errors.Is(err, ErrInvalidCredentials) {
			http.Error(w, "invalid credentials", http.StatusUnauthorized)
			return
		}
		m.log.Error("login failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if !verifyPassword(hash, in.Password) {
		http.Error(w, "invalid credentials", http.StatusUnauthorized)
		return
	}
	m.issueSession(w, r, p, http.StatusOK)
}
