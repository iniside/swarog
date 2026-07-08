package accounts

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/MicahParks/keyfunc/v3"
	"github.com/golang-jwt/jwt/v5"

	"gamebackend/api/accounts/accountsapi"
	"gamebackend/api/accounts/accountsevents"
	"gamebackend/bus"
	"gamebackend/opsapi"
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

// LoginEpic is the epic (EOS Connect / OIDC) login operation (AuthNone): it
// verifies an id_token and logs the player in, provisioning them on first sight
// (implicit registration) and emitting PlayerRegistered then. A missing id_token
// is StatusInvalid (→ 400); a rejected token is StatusUnauthorized (→ 401) — the
// same 400/401 the deleted handleEpicLogin returned. It is contributed as an
// operation only when the epic provider is configured, so s.epic is non-nil here.
func (s *service) LoginEpic(ctx context.Context, idToken string) (accountsapi.Session, error) {
	if idToken == "" {
		return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "id_token is required"}
	}
	subject, err := s.epic.verify(idToken)
	if err != nil {
		s.log.Warn("epic token rejected", "err", err)
		return accountsapi.Session{}, &opsapi.Error{Status: opsapi.StatusUnauthorized, Msg: "invalid id_token"}
	}

	p, created, err := s.store.findOrCreateExternal(ctx, "epic", subject, "epic:"+shortID(subject))
	if err != nil {
		s.log.Error("epic login failed", "err", err)
		return accountsapi.Session{}, err
	}
	if created {
		bus.Emit(s.bus, accountsevents.PlayerRegisteredEvent, accountsevents.PlayerRegistered{
			PlayerID: p.ID, DisplayName: p.DisplayName, Provider: "epic",
		})
	}
	return s.issueSession(ctx, p)
}

func shortID(s string) string {
	if len(s) > 8 {
		return s[:8]
	}
	return s
}
