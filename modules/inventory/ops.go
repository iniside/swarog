package inventory

import (
	"context"
	"encoding/json"
	"net/http"

	"gamebackend/lifecycle"
	"gamebackend/modules/inventory/inventoryapi"
	"gamebackend/modules/inventory/inventoryrpc"
	"gamebackend/opsapi"
)

// listCharacterReq carries the {id} path value of GET /inventory/character/{id}.
// Its JSON tag matches the generated listCharacterRequest envelope (characterID)
// so the SAME shape crosses the wire when dispatched over a RemoteBackend.
type listCharacterReq struct {
	CharacterID string `json:"characterID"`
}

// grantReq is the decoded request of POST /inventory/me/grant. It decodes the
// HTTP body's item_id/qty (the pre-migration public shape) but its OWN json tags
// (itemID/qty) match the generated grantRequest envelope, so a RemoteBackend
// re-marshal reaches the peer in the shape it decodes.
type grantReq struct {
	ItemID string `json:"itemID"`
	Qty    int    `json:"qty"`
}

// registerPlayerOps contributes the player operations: for each, an
// opsapi.Operation (the HTTP route + AuthPlayer + success code the gateway
// binds), an opsapi.OpBinding (HTTP body/path → typed request, and the typed
// response allocator), and an opsapi.LocalOp (the in-process invoker the gateway's
// LocalBackend dispatches, calling svc with the ctx the gateway stamped the
// verified player_id into). The invokers read NO identity themselves — svc reads
// it from ctx (opsapi.PlayerID), keeping the trust boundary in one place. Grant
// is contributed ONLY when devGrant is set (INVENTORY_DEV_GRANT), mirroring the
// old conditional route registration.
func registerPlayerOps(ctx *lifecycle.Context, svc *service, devGrant bool) {
	// GET /inventory/me → inventory.listMine (200).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: inventoryrpc.MethodListMine, Verb: "GET", Path: "/inventory/me",
		Auth: opsapi.AuthPlayer, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method:  inventoryrpc.MethodListMine,
		Decode:  func([]byte, map[string]string) (any, error) { return &struct{}{}, nil },
		NewResp: func() any { return &[]inventoryapi.Holding{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: inventoryrpc.MethodListMine,
		Invoke: func(ctx context.Context, _, resp any) error {
			list, err := svc.ListMine(ctx)
			if err != nil {
				return err
			}
			*resp.(*[]inventoryapi.Holding) = list
			return nil
		},
	})

	// GET /inventory/character/{id} → inventory.listCharacter (200). Ownership is
	// enforced inside svc.ListCharacter (Forbidden if the caller is not the owner).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: inventoryrpc.MethodListCharacter, Verb: "GET", Path: "/inventory/character/{id}",
		Auth: opsapi.AuthPlayer, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method: inventoryrpc.MethodListCharacter,
		Decode: func(_ []byte, path map[string]string) (any, error) {
			return &listCharacterReq{CharacterID: path["id"]}, nil
		},
		NewResp: func() any { return &[]inventoryapi.Holding{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: inventoryrpc.MethodListCharacter,
		Invoke: func(ctx context.Context, req, resp any) error {
			list, err := svc.ListCharacter(ctx, req.(*listCharacterReq).CharacterID)
			if err != nil {
				return err
			}
			*resp.(*[]inventoryapi.Holding) = list
			return nil
		},
	})

	if !devGrant {
		return
	}

	// POST /inventory/me/grant → inventory.grant (200), simulated IAP, dev-gated.
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: inventoryrpc.MethodGrant, Verb: "POST", Path: "/inventory/me/grant",
		Auth: opsapi.AuthPlayer, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method: inventoryrpc.MethodGrant,
		Decode: func(body []byte, _ map[string]string) (any, error) {
			// The public body shape is {"item_id":..,"qty":..}; translate it into the
			// wire-tagged grantReq (itemID/qty) so both the local assert and a remote
			// re-marshal agree.
			var in struct {
				ItemID string `json:"item_id"`
				Qty    int    `json:"qty"`
			}
			if err := json.Unmarshal(body, &in); err != nil {
				return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
			}
			return &grantReq{ItemID: in.ItemID, Qty: in.Qty}, nil
		},
		NewResp: func() any { return &[]inventoryapi.Holding{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: inventoryrpc.MethodGrant,
		Invoke: func(ctx context.Context, req, resp any) error {
			r := req.(*grantReq)
			list, err := svc.Grant(ctx, r.ItemID, r.Qty)
			if err != nil {
				return err
			}
			*resp.(*[]inventoryapi.Holding) = list
			return nil
		},
	})
}
