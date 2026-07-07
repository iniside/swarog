package characters

import (
	"context"
	"encoding/json"
	"net/http"

	"gamebackend/lifecycle"
	"gamebackend/modules/characters/charactersapi"
	"gamebackend/modules/characters/charactersplayerrpc"
	"gamebackend/opsapi"
)

// createReq is the decoded body of POST /characters. Its JSON tags match the
// generated createRequest envelope (name/class) so the SAME shape crosses the
// wire when the op is dispatched over a RemoteBackend.
type createReq struct {
	Name  string `json:"name"`
	Class string `json:"class"`
}

// deleteReq carries the {id} path value of DELETE /characters/{id}.
type deleteReq struct {
	id string
}

// registerPlayerOps contributes the three player operations: for each, an
// opsapi.Operation (the HTTP route + AuthPlayer + success code the gateway
// binds), an opsapi.OpBinding (HTTP body/path → typed request, and the typed
// response allocator), and an opsapi.LocalOp (the in-process invoker the gateway's
// LocalBackend dispatches, calling svc with the ctx the gateway stamped the
// verified player_id into). The invokers read NO identity themselves — svc reads
// it from ctx (opsapi.PlayerID), keeping the trust boundary in one place.
func registerPlayerOps(ctx *lifecycle.Context, svc *service) {
	// POST /characters → characters.create (201).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: charactersplayerrpc.MethodCreate, Verb: "POST", Path: "/characters",
		Auth: opsapi.AuthPlayer, Success: http.StatusCreated,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method: charactersplayerrpc.MethodCreate,
		Decode: func(body []byte, _ map[string]string) (any, error) {
			var r createReq
			if len(body) > 0 {
				if err := json.Unmarshal(body, &r); err != nil {
					return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
				}
			}
			return &r, nil
		},
		NewResp: func() any { return &charactersapi.Character{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: charactersplayerrpc.MethodCreate,
		Invoke: func(ctx context.Context, req, resp any) error {
			r := req.(*createReq)
			c, err := svc.Create(ctx, r.Name, r.Class)
			if err != nil {
				return err
			}
			*resp.(*charactersapi.Character) = c
			return nil
		},
	})

	// GET /characters → characters.list (200).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: charactersplayerrpc.MethodList, Verb: "GET", Path: "/characters",
		Auth: opsapi.AuthPlayer, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method:  charactersplayerrpc.MethodList,
		Decode:  func([]byte, map[string]string) (any, error) { return &struct{}{}, nil },
		NewResp: func() any { return &[]charactersapi.Character{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: charactersplayerrpc.MethodList,
		Invoke: func(ctx context.Context, _, resp any) error {
			list, err := svc.List(ctx)
			if err != nil {
				return err
			}
			*resp.(*[]charactersapi.Character) = list
			return nil
		},
	})

	// DELETE /characters/{id} → characters.delete (204, no body).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: charactersplayerrpc.MethodDelete, Verb: "DELETE", Path: "/characters/{id}",
		Auth: opsapi.AuthPlayer, Success: http.StatusNoContent,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method: charactersplayerrpc.MethodDelete,
		Decode: func(_ []byte, path map[string]string) (any, error) {
			return &deleteReq{id: path["id"]}, nil
		},
		NewResp: nil, // 204: no response body
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: charactersplayerrpc.MethodDelete,
		Invoke: func(ctx context.Context, req, _ any) error {
			return svc.Delete(ctx, req.(*deleteReq).id)
		},
	})
}
