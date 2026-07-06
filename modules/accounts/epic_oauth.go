package accounts

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"gamebackend/bus"
	"gamebackend/modules/accounts/accountsevents"
)

const stateTTL = 10 * time.Minute

// epicOAuth runs the Epic Account Services authorization-code flow. The backend
// is the confidential client (holds the secret); the browser only ever sees the
// redirect. A short-lived state store binds an in-flight authorization to the
// session that started it, so the callback knows whether to link or log in.
type epicOAuth struct {
	clientID     string
	clientSecret string
	redirectURI  string
	authorizeURL string
	tokenURL     string
	verifier     *oidcVerifier
	httpc        *http.Client

	mu     sync.Mutex
	states map[string]oauthState
}

type oauthState struct {
	sessionToken string // empty => login flow; set => link to this session's player
	createdAt    time.Time
}

func (o *epicOAuth) newState(sessionToken string) (string, error) {
	s, err := newToken()
	if err != nil {
		return "", err
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	for k, v := range o.states { // opportunistic GC
		if time.Since(v.createdAt) > stateTTL {
			delete(o.states, k)
		}
	}
	o.states[s] = oauthState{sessionToken: sessionToken, createdAt: time.Now()}
	return s, nil
}

func (o *epicOAuth) takeState(s string) (oauthState, bool) {
	o.mu.Lock()
	defer o.mu.Unlock()
	st, ok := o.states[s]
	if !ok {
		return oauthState{}, false
	}
	delete(o.states, s)
	if time.Since(st.createdAt) > stateTTL {
		return oauthState{}, false
	}
	return st, true
}

// exchangeCode swaps an authorization code for tokens and returns the id_token.
func (o *epicOAuth) exchangeCode(ctx context.Context, code string) (string, error) {
	form := url.Values{
		"grant_type":   {"authorization_code"},
		"code":         {code},
		"redirect_uri": {o.redirectURI},
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, o.tokenURL, strings.NewReader(form.Encode()))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.SetBasicAuth(o.clientID, o.clientSecret)

	resp, err := o.httpc.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return "", fmt.Errorf("token endpoint returned %d: %s", resp.StatusCode, body)
	}
	var tr struct {
		IDToken string `json:"id_token"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&tr); err != nil {
		return "", err
	}
	if tr.IDToken == "" {
		return "", errors.New("no id_token in token response (is the openid scope enabled for the app?)")
	}
	return tr.IDToken, nil
}

// handleEpicStart builds the authorize URL. Called via fetch with the user's
// bearer token: if present and valid, this becomes a LINK flow bound to that
// player; otherwise a plain login flow. Returns JSON so the page can redirect.
func (m *Module) handleEpicStart(w http.ResponseWriter, r *http.Request) {
	o := m.epicOAuth
	var sessionToken string
	if tok := bearerToken(r); tok != "" {
		if _, ok, _ := m.store.playerBySession(r.Context(), tok); ok {
			sessionToken = tok
		}
	}
	state, err := o.newState(sessionToken)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	authorize := o.authorizeURL + "?" + url.Values{
		"client_id":     {o.clientID},
		"response_type": {"code"},
		"scope":         {"openid basic_profile"},
		"redirect_uri":  {o.redirectURI},
		"state":         {state},
	}.Encode()
	writeJSON(w, http.StatusOK, map[string]string{"authorize_url": authorize})
}

// handleEpicCallback is where Epic redirects back. It exchanges the code,
// verifies the id_token, then links to the originating session or logs in.
func (m *Module) handleEpicCallback(w http.ResponseWriter, r *http.Request) {
	o := m.epicOAuth
	q := r.URL.Query()
	code, state := q.Get("code"), q.Get("state")
	if code == "" || state == "" {
		http.Error(w, "missing code or state", http.StatusBadRequest)
		return
	}
	st, ok := o.takeState(state)
	if !ok {
		http.Error(w, "invalid or expired state", http.StatusBadRequest)
		return
	}

	idToken, err := o.exchangeCode(r.Context(), code)
	if err != nil {
		m.log.Error("epic code exchange failed", "err", err)
		http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
		return
	}
	subject, err := o.verifier.verify(idToken)
	if err != nil {
		m.log.Warn("epic id_token rejected", "err", err)
		http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
		return
	}

	// Link flow: attach the Epic identity to the already-logged-in player.
	if st.sessionToken != "" {
		p, ok, _ := m.store.playerBySession(r.Context(), st.sessionToken)
		if !ok {
			http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
			return
		}
		if err := m.store.linkIdentity(r.Context(), p.ID, "epic", subject); err != nil && !errors.Is(err, ErrIdentityLinked) {
			m.log.Error("epic link failed", "err", err)
			http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
			return
		}
		http.Redirect(w, r, "/?epic=linked", http.StatusSeeOther)
		return
	}

	// Login flow: find or create a player for this Epic identity, mint a session,
	// hand the token back via the URL fragment for the page to pick up.
	p, created, err := m.store.findOrCreateExternal(r.Context(), "epic", subject, "epic:"+shortID(subject))
	if err != nil {
		m.log.Error("epic login failed", "err", err)
		http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
		return
	}
	if created {
		bus.Emit(m.bus, accountsevents.PlayerRegisteredEvent, accountsevents.PlayerRegistered{
			PlayerID: p.ID, DisplayName: p.DisplayName, Provider: "epic",
		})
	}
	token, err := m.store.newSession(r.Context(), p.ID)
	if err != nil {
		http.Redirect(w, r, "/?epic=error", http.StatusSeeOther)
		return
	}
	http.Redirect(w, r, "/#token="+token, http.StatusSeeOther)
}
