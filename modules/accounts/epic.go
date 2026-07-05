package accounts

import (
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/MicahParks/keyfunc/v3"
	"github.com/golang-jwt/jwt/v5"

	"gamebackend/bus"
	"gamebackend/modules/accounts/accountsevents"
)

// oidcVerifier verifies an OpenID-Connect ID token against a provider's JWKS.
// It is configured, not hardcoded: Epic is the first user, Google (also OIDC) is
// the known next one. The backend is a trusted verifier — it never holds the
// user's credentials, only checks the IdP's signed token (the EOS Connect model).
type oidcVerifier struct {
	audience     string // required "aud" — your EOS Client ID
	issuerPrefix string // "iss" must start with this
	jwks         keyfunc.Keyfunc
}

func newOIDCVerifier(jwksURL, issuerPrefix, audience string) (*oidcVerifier, error) {
	k, err := keyfunc.NewDefault([]string{jwksURL})
	if err != nil {
		return nil, err
	}
	return &oidcVerifier{audience: audience, issuerPrefix: issuerPrefix, jwks: k}, nil
}

// verify returns the token subject (for Epic, the Product User ID) if the token
// is authentic and valid: signature checked against the JWKS, alg != none,
// aud == audience, iss has the expected prefix, exp in the future.
func (v *oidcVerifier) verify(tokenStr string) (string, error) {
	claims := jwt.MapClaims{}
	tok, err := jwt.ParseWithClaims(tokenStr, claims, v.jwks.Keyfunc,
		jwt.WithValidMethods([]string{"RS256", "ES256"}), // excludes "none"
		jwt.WithAudience(v.audience),
		jwt.WithExpirationRequired(),
	)
	if err != nil {
		return "", err
	}
	if !tok.Valid {
		return "", errors.New("invalid token")
	}
	iss, err := claims.GetIssuer()
	if err != nil || !strings.HasPrefix(iss, v.issuerPrefix) {
		return "", fmt.Errorf("unexpected issuer %q", iss)
	}
	sub, err := claims.GetSubject()
	if err != nil || sub == "" {
		return "", errors.New("missing subject")
	}
	return sub, nil
}

// handleEpicLogin verifies an EOS Connect ID token and logs the player in,
// provisioning them on first sight (implicit registration).
func (m *Module) handleEpicLogin(w http.ResponseWriter, r *http.Request) {
	var in struct {
		IDToken string `json:"id_token"`
	}
	if !decodeJSON(w, r, &in) {
		return
	}
	if in.IDToken == "" {
		http.Error(w, "id_token is required", http.StatusBadRequest)
		return
	}

	subject, err := m.epic.verify(in.IDToken)
	if err != nil {
		m.log.Warn("epic token rejected", "err", err)
		http.Error(w, "invalid id_token", http.StatusUnauthorized)
		return
	}

	p, created, err := m.store.findOrCreateExternal(r.Context(), "epic", subject, "epic:"+shortID(subject))
	if err != nil {
		m.log.Error("epic login failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if created {
		bus.Emit(m.bus, accountsevents.PlayerRegisteredEvent, accountsevents.PlayerRegistered{
			PlayerID: p.ID, DisplayName: p.DisplayName, Provider: "epic",
		})
	}
	m.issueSession(w, r, p, http.StatusOK)
}

func shortID(s string) string {
	if len(s) > 8 {
		return s[:8]
	}
	return s
}
