package accounts

import (
	"context"
	"encoding/json"
	"net/http"

	"gamebackend/lifecycle"
	"gamebackend/modules/accounts/accountsapi"
	"gamebackend/modules/accounts/accountsauthrpc"
	"gamebackend/opsapi"
)

// registerReq is the decoded body of POST /accounts/register. Its JSON tags match
// the generated registerRequest envelope (email/password/displayName) so the SAME
// shape crosses the wire when the op is dispatched over a RemoteBackend.
type registerReq struct {
	Email       string `json:"email"`
	Password    string `json:"password"`
	DisplayName string `json:"displayName"`
}

// loginReq is the decoded body of POST /accounts/login (email/password), matching
// the generated loginRequest envelope.
type loginReq struct {
	Email    string `json:"email"`
	Password string `json:"password"`
}

// loginEpicReq carries the id_token of POST /accounts/login/epic. The PUBLIC body
// shape is {"id_token":..} (the pre-migration contract) but its OWN json tag
// (idToken) matches the generated loginEpicRequest envelope, so a RemoteBackend
// re-marshal reaches the peer in the shape it decodes.
type loginEpicReq struct {
	IDToken string `json:"idToken"`
}

// meResp is the typed response of GET /accounts/me. It embeds Player (so its
// player_id/display_name flatten to the top level) plus the identities list —
// exactly the {player_id, display_name, identities} shape the deleted handleMe
// wrote.
type meResp struct {
	accountsapi.Player
	Identities []accountsapi.Identity `json:"identities"`
}

// registerPlayerOps contributes the accounts player operations: for each, an
// opsapi.Operation (the HTTP route + auth + success code the gateway binds), an
// opsapi.OpBinding (HTTP body/path → typed request, and the typed response
// allocator), and an opsapi.LocalOp (the in-process invoker the gateway's
// LocalBackend dispatches). register/login/loginEpic are AuthNone — they CREATE a
// session, so the gateway does NOT pre-verify a bearer; the op itself returns the
// auth outcome (bad credentials → StatusUnauthorized → 401). me is AuthPlayer — the
// gateway verifies the bearer and injects the player_id, which svc.Me reads from
// ctx (opsapi.PlayerID). register+login are contributed only under devAuth and
// loginEpic only when the epic provider is up, mirroring the old conditional route
// registration.
func registerPlayerOps(ctx *lifecycle.Context, svc *service, devAuth, epicEnabled bool) {
	if devAuth {
		// POST /accounts/register → accounts.register (201), dev-gated.
		ctx.Contribute(opsapi.Slot, opsapi.Operation{
			Method: accountsauthrpc.MethodRegister, Verb: "POST", Path: "/accounts/register",
			Auth: opsapi.AuthNone, Success: http.StatusCreated,
		})
		ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
			Method: accountsauthrpc.MethodRegister,
			Decode: func(body []byte, _ map[string]string) (any, error) {
				var r registerReq
				if err := json.Unmarshal(body, &r); err != nil {
					return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
				}
				return &r, nil
			},
			NewResp: func() any { return &accountsapi.Session{} },
		})
		ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
			Method: accountsauthrpc.MethodRegister,
			Invoke: func(ctx context.Context, req, resp any) error {
				r := req.(*registerReq)
				s, err := svc.Register(ctx, r.Email, r.Password, r.DisplayName)
				if err != nil {
					return err
				}
				*resp.(*accountsapi.Session) = s
				return nil
			},
		})

		// POST /accounts/login → accounts.login (200), dev-gated.
		ctx.Contribute(opsapi.Slot, opsapi.Operation{
			Method: accountsauthrpc.MethodLogin, Verb: "POST", Path: "/accounts/login",
			Auth: opsapi.AuthNone, Success: http.StatusOK,
		})
		ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
			Method: accountsauthrpc.MethodLogin,
			Decode: func(body []byte, _ map[string]string) (any, error) {
				var r loginReq
				if err := json.Unmarshal(body, &r); err != nil {
					return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
				}
				return &r, nil
			},
			NewResp: func() any { return &accountsapi.Session{} },
		})
		ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
			Method: accountsauthrpc.MethodLogin,
			Invoke: func(ctx context.Context, req, resp any) error {
				r := req.(*loginReq)
				s, err := svc.Login(ctx, r.Email, r.Password)
				if err != nil {
					return err
				}
				*resp.(*accountsapi.Session) = s
				return nil
			},
		})
	}

	if epicEnabled {
		// POST /accounts/login/epic → accounts.loginEpic (200), epic-gated.
		ctx.Contribute(opsapi.Slot, opsapi.Operation{
			Method: accountsauthrpc.MethodLoginEpic, Verb: "POST", Path: "/accounts/login/epic",
			Auth: opsapi.AuthNone, Success: http.StatusOK,
		})
		ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
			Method: accountsauthrpc.MethodLoginEpic,
			Decode: func(body []byte, _ map[string]string) (any, error) {
				// The public body shape is {"id_token":..}; translate it into the
				// wire-tagged loginEpicReq (idToken) so both the local assert and a
				// remote re-marshal agree.
				var in struct {
					IDToken string `json:"id_token"`
				}
				if err := json.Unmarshal(body, &in); err != nil {
					return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
				}
				return &loginEpicReq{IDToken: in.IDToken}, nil
			},
			NewResp: func() any { return &accountsapi.Session{} },
		})
		ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
			Method: accountsauthrpc.MethodLoginEpic,
			Invoke: func(ctx context.Context, req, resp any) error {
				s, err := svc.LoginEpic(ctx, req.(*loginEpicReq).IDToken)
				if err != nil {
					return err
				}
				*resp.(*accountsapi.Session) = s
				return nil
			},
		})
	}

	// GET /accounts/me → accounts.me (200), AuthPlayer. Always contributed — the
	// gateway verifies the bearer and injects the player_id before dispatch.
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: accountsauthrpc.MethodMe, Verb: "GET", Path: "/accounts/me",
		Auth: opsapi.AuthPlayer, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method:  accountsauthrpc.MethodMe,
		Decode:  func([]byte, map[string]string) (any, error) { return &struct{}{}, nil },
		NewResp: func() any { return &meResp{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: accountsauthrpc.MethodMe,
		Invoke: func(ctx context.Context, _, resp any) error {
			p, ids, err := svc.Me(ctx)
			if err != nil {
				return err
			}
			*resp.(*meResp) = meResp{Player: p, Identities: ids}
			return nil
		},
	})
}
