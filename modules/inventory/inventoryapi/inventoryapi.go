// Package inventoryapi is the inventory module's pure, transport-free capability
// contract: the canonical Holdings interface the module exposes as player
// operations. It imports only context — no edge, no transport — so it is the
// clean codegen INPUT for rpcgen, which reads it to generate the transport glue
// in the sibling inventoryrpc package.
//
// Domain consumers do NOT import this package: they keep their own local
// structural interface (CLAUDE.md rule 4). It is reached only by the generated
// glue (inventoryrpc) + the remote stub — the provider-owned contract surface,
// same precedent as each module's <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Holdings -prefix inventory -out ../inventoryrpc/inventoryrpc_gen.go
package inventoryapi

import "context"

// Holding is one item stack an owner holds. It lives here (not in the impl
// package) because it is a return type of the Holdings capability, so the
// generated glue must be able to name it; the inventory module aliases it as its
// own Holding. The JSON tags are the player-facing wire shape (owner_type /
// owner_id / item_id / item_name / quantity) — unchanged from the pre-migration
// handler responses.
type Holding struct {
	OwnerType string `json:"owner_type"`
	OwnerID   string `json:"owner_id"`
	ItemID    string `json:"item_id"`
	ItemName  string `json:"item_name"`
	Quantity  int    `json:"quantity"`
}

// Holdings is the inventory module's player-facing capability: the three
// operations a player performs against their OWN inventory. Each takes its caller
// identity from ctx (opsapi.PlayerID, injected by the gateway after bearer
// verification), NEVER as an argument — so a client cannot read or mutate another
// player's inventory. ListCharacter additionally authorizes the character against
// the caller (a character the caller does not own is a Forbidden outcome). The
// inventory service implements it exactly; the gateway/edge glue is generated
// from it.
type Holdings interface {
	// ListMine returns the caller's own (player-owned) holdings.
	ListMine(ctx context.Context) (holdings []Holding, err error)
	// ListCharacter returns a character's holdings, but only if the character is
	// owned by the caller — otherwise a Forbidden outcome (never another player's
	// inventory). A genuinely unknown character is NotFound; an ownership-lookup
	// transport failure is Unavailable.
	ListCharacter(ctx context.Context, characterID string) (holdings []Holding, err error)
	// Grant adds qty of itemID to the caller's own (player-owned) inventory — the
	// simulated-IAP path, gated by INVENTORY_DEV_GRANT. A non-positive qty or an
	// unknown item is an Invalid outcome.
	Grant(ctx context.Context, itemID string, qty int) (holdings []Holding, err error)
}
