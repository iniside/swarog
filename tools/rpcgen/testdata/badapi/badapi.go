// Package badapi holds interfaces with signature shapes rpcgen must REJECT at
// generate time (rather than emit broken code). They are valid Go — they compile
// — but each violates a scope rule; the rpcgen test drives the generator at each
// and asserts a clear error. It is never used as real glue input.
package badapi

import "context"

// IfaceParam has an interface-typed parameter (not the trailing error).
type IfaceParam interface {
	M(ctx context.Context, x interface{ Foo() }) error
}

// IfaceReturn has an interface-typed (non-error) return.
type IfaceReturn interface {
	M(ctx context.Context, id string) (interface{ Foo() }, error)
}

// ChanParam has a channel-typed parameter.
type ChanParam interface {
	M(ctx context.Context, c chan int) error
}

// FuncParam has a func-typed parameter.
type FuncParam interface {
	M(ctx context.Context, f func()) error
}

// NoCtx omits the leading context.Context.
type NoCtx interface {
	M(x string) error
}

// NoErr omits the trailing error result.
type NoErr interface {
	M(ctx context.Context, x string) string
}

// Generic is a type-parameterised (generic) interface.
type Generic[T any] interface {
	M(ctx context.Context, x T) error
}
