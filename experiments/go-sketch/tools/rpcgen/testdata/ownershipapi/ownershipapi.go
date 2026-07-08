// Package ownershipapi is a SELF-CONTAINED rpcgen fixture: a pure capability
// interface (imports only context + its own pure types) that exercises the
// signature shapes rpcgen supports — a string param, string + bool returns, and
// a struct return. It is the codegen input for the golden ownershiprpc package;
// it is NOT wired into any real module.
//
//go:generate go run gamebackend/tools/rpcgen -iface Ownership -prefix ownership -out ../ownershiprpc/ownershiprpc_gen.go
package ownershipapi

import "context"

// Info is a pure struct return type, standing in for a real module's struct DTO
// (e.g. adminapi.ItemData) to prove struct returns marshal cleanly.
type Info struct {
	Name  string `json:"name"`
	Level int    `json:"level"`
}

// Ownership is the capability interface. Methods are declared out of alphabetical
// order on purpose so the golden proves rpcgen sorts them.
type Ownership interface {
	// OwnerOf resolves a character's owning player. (string, bool, error).
	OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error)
	// Describe returns a struct. (Info, error) with an unnamed result.
	Describe(ctx context.Context, characterID string) (Info, error)
}
