// Command rpcgen is a PRAGMATIC Go-interface -> RPC-glue generator, NOT a
// general-purpose IDL compiler. Given a provider's pure capability interface in a
// <module>api package, it generates the transport glue in a sibling <module>rpc
// package: per-method request/response wire envelopes, a Client that implements
// the interface over an opsapi.Caller, and a RegisterServer that installs one
// edge adapter per method. The single generated source replaces the ~10
// hand-kept pieces per method (client stub + mirrored DTOs + method strings +
// provider adapter + wire_contract byte-comparisons) with one deterministic
// artifact, so wire drift between the two sides is structurally impossible.
//
// It mirrors tools/topiccheck: one golang.org/x/tools/go/packages load gives a
// shared set of go/types objects, and the interface's methods are read straight
// off the *types.Interface via go/types.
//
// Scope. It handles ONLY the signature shapes this repo actually uses:
//
//   - first param must be context.Context (not marshaled; the server adapter
//     calls impl with opsapi.WithPlayerID(context.Background(), envelope.Identity)
//     so the caller's verified player_id — stamped by the gateway into the edge
//     envelope — is the ONLY identity a remote op sees, read from ctx),
//   - last result must be error (stripped from the wire; carried as an opsapi
//     Status in the response envelope),
//   - remaining params/results may be basic types, strings, bools, structs,
//     pointers, slices, arrays and maps of those (all JSON-marshaled).
//
// Anything else — an interface-typed param/result (other than the trailing
// error), a generic (type-parameterised) interface, a channel, or a func — is a
// GENERATE-TIME error, never silently-broken code.
//
// Determinism (the -check drift gate must be toolchain-stable): methods are
// sorted by name, imports are split into a fixed stdlib / gamebackend grouping
// and sorted, and the whole output is run through go/format before it is written
// or compared. -check regenerates to memory, format-normalizes BOTH sides
// identically, and diffs — so it does not false-positive across Go patch
// versions.
//
// Usage (typically from a //go:generate directive in the <module>api package):
//
//	rpcgen -iface Ownership -prefix characters -out ../charactersrpc/charactersrpc_gen.go [-pkg charactersrpc] [-check] [pattern]
//
// pattern defaults to "." (the api package the directive lives in). -pkg
// defaults to the base name of the -out directory.
package main

import (
	"bytes"
	"flag"
	"fmt"
	"go/ast"
	"go/format"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"golang.org/x/tools/go/packages"
)

const (
	edgePkgPath   = "gamebackend/edge"
	opsapiPkgPath = "gamebackend/opsapi"
)

// loadMode loads names, full type info and syntax for the target package and its
// deps, so the interface type and every referenced type resolve to real
// go/types objects with their owning packages.
const loadMode = packages.NeedName |
	packages.NeedTypes |
	packages.NeedTypesInfo |
	packages.NeedSyntax |
	packages.NeedDeps |
	packages.NeedFiles |
	packages.NeedImports

func main() {
	iface := flag.String("iface", "", "name of the capability interface to generate glue for (required)")
	prefix := flag.String("prefix", "", "method-name prefix, e.g. \"characters\" -> \"characters.ownerOf\" (required)")
	out := flag.String("out", "", "output .go file path (required)")
	pkgName := flag.String("pkg", "", "output package name (default: base name of -out's directory)")
	check := flag.Bool("check", false, "generate to memory and diff against -out; exit non-zero on drift (do not write)")
	flag.Parse()

	if *iface == "" || *prefix == "" || *out == "" {
		fmt.Fprintln(os.Stderr, "rpcgen: -iface, -prefix and -out are required")
		flag.Usage()
		os.Exit(2)
	}

	pattern := "."
	if args := flag.Args(); len(args) > 0 {
		pattern = args[0]
	}

	outPkg := *pkgName
	if outPkg == "" {
		outPkg = filepath.Base(filepath.Dir(*out))
	}

	src, err := run(pattern, *iface, *prefix, outPkg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "rpcgen: %v\n", err)
		os.Exit(1)
	}

	if *check {
		if err := checkAgainst(*out, src); err != nil {
			fmt.Fprintf(os.Stderr, "rpcgen: -check: %v\n", err)
			os.Exit(1)
		}
		fmt.Printf("rpcgen: %s is up to date\n", *out)
		return
	}

	if err := os.WriteFile(*out, src, 0o600); err != nil {
		fmt.Fprintf(os.Stderr, "rpcgen: write %s: %v\n", *out, err)
		os.Exit(1)
	}
	fmt.Printf("rpcgen: wrote %s\n", *out)
}

// run loads the package, resolves the interface, and returns the gofmt'd glue.
func run(pattern, ifaceName, prefix, outPkg string) ([]byte, error) {
	pkgs, err := packages.Load(&packages.Config{Mode: loadMode}, pattern)
	if err != nil {
		return nil, fmt.Errorf("load %q: %w", pattern, err)
	}
	if packages.PrintErrors(pkgs) > 0 {
		return nil, fmt.Errorf("package %q has errors", pattern)
	}

	pkg, iface, err := findInterface(pkgs, ifaceName)
	if err != nil {
		return nil, err
	}
	binds, err := loadBindings(pkg)
	if err != nil {
		return nil, err
	}
	return generate(pkg.Types, iface, ifaceName, prefix, outPkg, binds)
}

// findInterface locates the named interface type across the loaded packages,
// rejecting a generic (type-parameterised) interface. It returns the whole
// *packages.Package (not just its *types.Package) so the caller can also read the
// package's HTTPBindings var off its syntax.
func findInterface(pkgs []*packages.Package, ifaceName string) (*packages.Package, *types.Interface, error) {
	for _, pkg := range pkgs {
		if pkg.Types == nil {
			continue
		}
		obj := pkg.Types.Scope().Lookup(ifaceName)
		if obj == nil {
			continue
		}
		named, ok := obj.Type().(*types.Named)
		if !ok {
			return nil, nil, fmt.Errorf("%s is not a named type", ifaceName)
		}
		if named.TypeParams() != nil && named.TypeParams().Len() > 0 {
			return nil, nil, fmt.Errorf("%s is generic; generic interfaces are not supported", ifaceName)
		}
		iface, ok := named.Underlying().(*types.Interface)
		if !ok {
			return nil, nil, fmt.Errorf("%s is not an interface", ifaceName)
		}
		return pkg, iface, nil
	}
	return nil, nil, fmt.Errorf("interface %q not found in %d loaded package(s)", ifaceName, len(pkgs))
}

// httpBind is one method's HTTP-surface declaration, read from the api package's
// `var HTTPBindings map[string]opsapi.HTTPBind`. Values are captured as SOURCE
// TEXT the generator re-emits (verb/path/success) or maps (path/body arg
// placement) — no go/types constant evaluation, so the read is robust and the
// generated Operation literal is a faithful copy of the declaration.
type httpBind struct {
	Verb     string            // string literal, e.g. "POST"
	Path     string            // string literal, e.g. "/characters/{id}"
	AuthName string            // selector Sel name, "AuthPlayer" | "AuthNone"
	Success  string            // int literal source text, e.g. "201"
	PathArgs map[string]string // param name -> path wildcard name
	BodyName map[string]string // param name -> external body JSON key (override)
}

// loadBindings reads the optional `var HTTPBindings = map[string]opsapi.HTTPBind{…}`
// from the api package's syntax and returns it keyed by Go method name. Absent (an
// api package that exposes no HTTP operations, e.g. the ownership/sessions faces)
// it returns an empty map — those interfaces generate wire-only glue, exactly as
// before. It reads AST literals directly (no eval), so Verb/Path/Success MUST be
// plain string/int literals and Auth an opsapi.AuthNone/AuthPlayer selector.
func loadBindings(pkg *packages.Package) (map[string]httpBind, error) {
	for _, f := range pkg.Syntax {
		for _, decl := range f.Decls {
			gd, ok := decl.(*ast.GenDecl)
			if !ok || gd.Tok != token.VAR {
				continue
			}
			for _, spec := range gd.Specs {
				vs, ok := spec.(*ast.ValueSpec)
				if !ok {
					continue
				}
				for i, name := range vs.Names {
					if name.Name != "HTTPBindings" || i >= len(vs.Values) {
						continue
					}
					lit, ok := vs.Values[i].(*ast.CompositeLit)
					if !ok {
						return nil, fmt.Errorf("HTTPBindings must be a map literal")
					}
					return parseBindingsMap(lit)
				}
			}
		}
	}
	return map[string]httpBind{}, nil
}

// parseBindingsMap turns the `map[string]opsapi.HTTPBind{ "Method": {…} }` literal
// into httpBind values keyed by method name.
func parseBindingsMap(lit *ast.CompositeLit) (map[string]httpBind, error) {
	out := map[string]httpBind{}
	for _, elt := range lit.Elts {
		kv, ok := elt.(*ast.KeyValueExpr)
		if !ok {
			continue
		}
		methodName, err := strLit(kv.Key)
		if err != nil {
			return nil, fmt.Errorf("HTTPBindings key: %w", err)
		}
		val, ok := kv.Value.(*ast.CompositeLit)
		if !ok {
			return nil, fmt.Errorf("HTTPBindings[%q] must be a struct literal", methodName)
		}
		b, err := parseBind(val)
		if err != nil {
			return nil, fmt.Errorf("HTTPBindings[%q]: %w", methodName, err)
		}
		out[methodName] = b
	}
	return out, nil
}

// parseBind reads one opsapi.HTTPBind{…} struct literal's fields.
func parseBind(lit *ast.CompositeLit) (httpBind, error) {
	b := httpBind{}
	for _, elt := range lit.Elts {
		kv, ok := elt.(*ast.KeyValueExpr)
		if !ok {
			continue
		}
		key, ok := kv.Key.(*ast.Ident)
		if !ok {
			continue
		}
		switch key.Name {
		case "Verb":
			s, err := strLit(kv.Value)
			if err != nil {
				return b, fmt.Errorf("verb: %w", err)
			}
			b.Verb = s
		case "Path":
			s, err := strLit(kv.Value)
			if err != nil {
				return b, fmt.Errorf("path: %w", err)
			}
			b.Path = s
		case "Success":
			blit, ok := kv.Value.(*ast.BasicLit)
			if !ok || blit.Kind != token.INT {
				return b, fmt.Errorf("success must be a plain int literal")
			}
			b.Success = blit.Value
		case "Auth":
			sel, ok := kv.Value.(*ast.SelectorExpr)
			if !ok {
				return b, fmt.Errorf("auth must be opsapi.AuthNone or opsapi.AuthPlayer")
			}
			b.AuthName = sel.Sel.Name
		case "PathArgs":
			m, err := parseStringMap(kv.Value)
			if err != nil {
				return b, fmt.Errorf("pathArgs: %w", err)
			}
			b.PathArgs = m
		case "BodyNames":
			m, err := parseStringMap(kv.Value)
			if err != nil {
				return b, fmt.Errorf("bodyNames: %w", err)
			}
			b.BodyName = m
		}
	}
	if b.Verb == "" || b.Path == "" || b.Success == "" || b.AuthName == "" {
		return b, fmt.Errorf("verb, path, auth and success are all required")
	}
	return b, nil
}

// parseStringMap reads a map[string]string{…} literal into a Go map.
func parseStringMap(expr ast.Expr) (map[string]string, error) {
	lit, ok := expr.(*ast.CompositeLit)
	if !ok {
		return nil, fmt.Errorf("must be a map literal")
	}
	m := map[string]string{}
	for _, elt := range lit.Elts {
		kv, ok := elt.(*ast.KeyValueExpr)
		if !ok {
			continue
		}
		k, err := strLit(kv.Key)
		if err != nil {
			return nil, err
		}
		v, err := strLit(kv.Value)
		if err != nil {
			return nil, err
		}
		m[k] = v
	}
	return m, nil
}

// strLit unquotes a string-literal AST expression.
func strLit(expr ast.Expr) (string, error) {
	lit, ok := expr.(*ast.BasicLit)
	if !ok || lit.Kind != token.STRING {
		return "", fmt.Errorf("expected a string literal")
	}
	return strconv.Unquote(lit.Value)
}

// method is one validated interface method reduced to what codegen needs.
type method struct {
	Name string  // Go method name, e.g. "OwnerOf"
	Wire string  // wire method string, e.g. "characters.ownerOf"
	Args []field // params after ctx, in order
	Rets []field // results before the trailing error, in order

	// Bound is set when an HTTPBindings entry names this method: the generator then
	// emits the gateway binding (Decode/Encode/Operations) in addition to the wire
	// client/server. The four fields below are copied from the HTTPBind so the
	// generated opsapi.Operation literal is a single source of truth with the wire.
	Bound    bool
	Verb     string // HTTP verb literal, e.g. "POST"
	Path     string // HTTP path pattern, e.g. "/characters/{id}"
	AuthName string // "AuthPlayer" | "AuthNone" (emitted as <opsapi>.<AuthName>)
	Success  string // HTTP success status, an int literal source text, e.g. "201"
}

// field is one wire-envelope field: its exported Go name, JSON tag, and the Go
// source spelling of its type (with package qualifiers).
type field struct {
	GoName string
	JSON   string
	Type   string

	// Param is the original interface parameter name (empty for a return or an
	// unnamed param), used to look the field up in an HTTPBind and to reference it
	// off the decoded request in a generated invoker.
	Param string
	// FromPath marks an argument sourced from a path wildcard rather than the body;
	// Wildcard is that wildcard's name (e.g. "id" for "/characters/{id}"). The
	// generated Decode reads path[Wildcard] into this field instead of unmarshaling.
	FromPath bool
	Wildcard string
}

// generate builds and gofmt-normalizes the glue source for iface. binds maps a Go
// method name to its HTTP binding (empty for a wire-only interface); a bound
// method additionally gets a generated gateway binding.
func generate(apiPkg *types.Package, iface *types.Interface, ifaceName, prefix, outPkg string, binds map[string]httpBind) ([]byte, error) {
	// imports maps an import path to the package name to reference it by. Seed
	// with the always-present imports; type rendering adds the rest.
	imports := map[string]string{
		"context":       "context",
		"encoding/json": "json",
		edgePkgPath:     "edge",
		opsapiPkgPath:   "opsapi",
		apiPkg.Path():   apiPkg.Name(),
	}
	qual := func(p *types.Package) string {
		if p == nil {
			return ""
		}
		imports[p.Path()] = p.Name()
		return p.Name()
	}

	var methods []method
	anyBound := false
	for i := 0; i < iface.NumMethods(); i++ {
		fn := iface.Method(i)
		bind, ok := binds[fn.Name()]
		var bp *httpBind
		if ok {
			bp = &bind
		}
		m, err := buildMethod(fn, prefix, qual, bp)
		if err != nil {
			return nil, fmt.Errorf("method %s: %w", fn.Name(), err)
		}
		if m.Bound {
			anyBound = true
		}
		methods = append(methods, m)
	}
	sort.Slice(methods, func(i, j int) bool { return methods[i].Name < methods[j].Name })

	var b strings.Builder
	fmt.Fprintf(&b, "// Code generated by rpcgen; DO NOT EDIT.\n\n")
	fmt.Fprintf(&b, "package %s\n\n", outPkg)
	writeImports(&b, imports)

	apiName := imports[apiPkg.Path()]

	// Method-name constants.
	b.WriteString("// Method names are the wire identifiers the client sends and the server\n")
	b.WriteString("// registers under.\nconst (\n")
	for _, m := range methods {
		fmt.Fprintf(&b, "\tMethod%s = %q\n", m.Name, m.Wire)
	}
	b.WriteString(")\n\n")

	// Per-method wire envelopes.
	for _, m := range methods {
		lc := lowerFirst(m.Name)
		fmt.Fprintf(&b, "// %sRequest is the wire request envelope for %s.\n", lc, m.Name)
		fmt.Fprintf(&b, "type %sRequest struct {\n", lc)
		for _, a := range m.Args {
			fmt.Fprintf(&b, "\t%s %s `json:%q`\n", a.GoName, a.Type, a.JSON)
		}
		b.WriteString("}\n\n")

		fmt.Fprintf(&b, "// %sResponse is the wire response envelope for %s. Status/Err carry the\n", lc, m.Name)
		fmt.Fprintf(&b, "// operation outcome (opsapi taxonomy); the fields carry the return values.\n")
		fmt.Fprintf(&b, "type %sResponse struct {\n", lc)
		fmt.Fprintf(&b, "\tStatus %s.Status `json:\"status\"`\n", imports[opsapiPkgPath])
		b.WriteString("\tErr string `json:\"err,omitempty\"`\n")
		for _, r := range m.Rets {
			fmt.Fprintf(&b, "\t%s %s `json:%q`\n", r.GoName, r.Type, r.JSON)
		}
		b.WriteString("}\n\n")
	}

	// Client.
	opsName := imports[opsapiPkgPath]
	fmt.Fprintf(&b, "// Client implements %s.%s over an %s.Caller, marshaling each call\n", apiName, ifaceName, opsName)
	fmt.Fprintf(&b, "// into its wire envelope. It is the split-topology client; in the monolith the\n")
	fmt.Fprintf(&b, "// real service is called directly.\n")
	fmt.Fprintf(&b, "type Client struct {\n\tcaller %s.Caller\n}\n\n", opsName)
	fmt.Fprintf(&b, "// NewClient returns a Client that calls through caller.\n")
	fmt.Fprintf(&b, "func NewClient(caller %s.Caller) *Client { return &Client{caller: caller} }\n\n", opsName)
	fmt.Fprintf(&b, "var _ %s.%s = (*Client)(nil)\n\n", apiName, ifaceName)

	for _, m := range methods {
		writeClientMethod(&b, m, opsName)
	}

	// Server.
	fmt.Fprintf(&b, "// Registrar is the subset of *%s.Server the generated adapters install onto:\n", imports[edgePkgPath])
	fmt.Fprintf(&b, "// one identity-aware handler per method. *%s.Server satisfies it.\n", imports[edgePkgPath])
	fmt.Fprintf(&b, "type Registrar interface {\n\tHandleIdentity(method string, h %s.IdentityHandler)\n}\n\n", imports[edgePkgPath])
	fmt.Fprintf(&b, "// RegisterServer installs one edge adapter per method of impl onto reg. Each\n")
	fmt.Fprintf(&b, "// adapter unmarshals the request, calls impl with a context carrying the request\n")
	fmt.Fprintf(&b, "// envelope's Identity as the verified caller player_id (%s.WithPlayerID) — the\n", opsName)
	fmt.Fprintf(&b, "// trust boundary: identity is read ONLY from the (mutually authenticated) envelope,\n")
	fmt.Fprintf(&b, "// never a client-supplied field — and marshals the response envelope, folding a\n")
	fmt.Fprintf(&b, "// returned error into Status/Err via %s.StatusOf.\n", opsName)
	fmt.Fprintf(&b, "func RegisterServer(reg Registrar, impl %s.%s) {\n", apiName, ifaceName)
	for _, m := range methods {
		writeServerAdapter(&b, m, opsName)
	}
	b.WriteString("}\n")

	// Gateway binding (only when at least one method declares an HTTPBind): per
	// bound method a Decode (HTTP body/path -> wire request envelope) and an Encode
	// (wire response envelope -> external HTTP body + Status), plus one Operations
	// map the module contributes to the gateway slots. This makes LocalBackend and
	// RemoteBackend consume the SAME wire envelopes, so RemoteBackend is correct for
	// every op shape, with the external HTTP body unchanged.
	if anyBound {
		writeGatewayBinding(&b, methods, apiName, ifaceName, opsName)
	}

	formatted, err := format.Source([]byte(b.String()))
	if err != nil {
		return nil, fmt.Errorf("gofmt generated source: %w\n--- source ---\n%s", err, b.String())
	}
	return formatted, nil
}

// writeClientMethod emits one Client method: marshal args -> Call -> map Status.
func writeClientMethod(b *strings.Builder, m method, opsName string) {
	lc := lowerFirst(m.Name)

	// Signature with positional names a0.. and r0.., trailing err.
	var params []string
	for i := range m.Args {
		params = append(params, fmt.Sprintf("a%d %s", i, m.Args[i].Type))
	}
	var results []string
	for i := range m.Rets {
		results = append(results, fmt.Sprintf("r%d %s", i, m.Rets[i].Type))
	}
	results = append(results, "err error")

	fmt.Fprintf(b, "// %s implements the capability by calling %s over the transport.\n", m.Name, m.Wire)
	fmt.Fprintf(b, "func (c *Client) %s(ctx context.Context, %s) (%s) {\n",
		m.Name, strings.Join(params, ", "), strings.Join(results, ", "))

	// Build request.
	fmt.Fprintf(b, "\treq := %sRequest{", lc)
	var assigns []string
	for i, a := range m.Args {
		assigns = append(assigns, fmt.Sprintf("%s: a%d", a.GoName, i))
	}
	b.WriteString(strings.Join(assigns, ", "))
	b.WriteString("}\n")
	fmt.Fprintf(b, "\tvar resp %sResponse\n", lc)
	fmt.Fprintf(b, "\tif err = c.caller.Call(ctx, Method%s, &req, &resp); err != nil {\n\t\treturn\n\t}\n", m.Name)
	fmt.Fprintf(b, "\tif resp.Status != %s.StatusOK {\n", opsName)
	fmt.Fprintf(b, "\t\terr = &%s.Error{Status: resp.Status, Msg: resp.Err}\n\t\treturn\n\t}\n", opsName)
	for i, r := range m.Rets {
		fmt.Fprintf(b, "\tr%d = resp.%s\n", i, r.GoName)
	}
	b.WriteString("\treturn\n}\n\n")
}

// writeServerAdapter emits one reg.Handle(...) block inside RegisterServer.
func writeServerAdapter(b *strings.Builder, m method, opsName string) {
	lc := lowerFirst(m.Name)
	fmt.Fprintf(b, "\treg.HandleIdentity(Method%s, func(identity string, reqPayload []byte) ([]byte, error) {\n", m.Name)
	fmt.Fprintf(b, "\t\tvar req %sRequest\n", lc)
	b.WriteString("\t\tif err := json.Unmarshal(reqPayload, &req); err != nil {\n\t\t\treturn nil, err\n\t\t}\n")

	// Call impl with the caller identity injected into ctx (from the envelope).
	var lhs []string
	for i := range m.Rets {
		lhs = append(lhs, fmt.Sprintf("r%d", i))
	}
	lhs = append(lhs, "err")
	var callArgs []string
	for _, a := range m.Args {
		callArgs = append(callArgs, "req."+a.GoName)
	}
	callArgList := ""
	if len(callArgs) > 0 {
		callArgList = ", " + strings.Join(callArgs, ", ")
	}
	fmt.Fprintf(b, "\t\t%s := impl.%s(%s.WithPlayerID(context.Background(), identity)%s)\n", strings.Join(lhs, ", "), m.Name, opsName, callArgList)

	fmt.Fprintf(b, "\t\tresp := %sResponse{}\n", lc)
	b.WriteString("\t\tif err != nil {\n")
	fmt.Fprintf(b, "\t\t\tresp.Status = %s.StatusOf(err)\n", opsName)
	b.WriteString("\t\t\tresp.Err = err.Error()\n")
	b.WriteString("\t\t} else {\n")
	fmt.Fprintf(b, "\t\t\tresp.Status = %s.StatusOK\n", opsName)
	for i, r := range m.Rets {
		fmt.Fprintf(b, "\t\t\tresp.%s = r%d\n", r.GoName, i)
	}
	b.WriteString("\t\t}\n")
	b.WriteString("\t\treturn json.Marshal(resp)\n")
	b.WriteString("\t})\n")
}

// writeGatewayBinding emits the generated gateway binding for the bound methods: a
// decodeX (HTTP body/path -> wire request envelope) and an encodeX (wire response
// envelope -> external HTTP body + Status) per method, and one Operations(impl)
// the module contributes to the gateway slots. Because both LocalBackend and
// RemoteBackend produce/consume these SAME wire envelopes, remote dispatch is
// correct for every op shape while the external HTTP body is unchanged.
func writeGatewayBinding(b *strings.Builder, methods []method, apiName, ifaceName, opsName string) {
	for _, m := range methods {
		if !m.Bound {
			continue
		}
		writeDecode(b, m, opsName)
		writeEncode(b, m, opsName)
	}

	fmt.Fprintf(b, "// Operations returns the gateway contributions for each HTTP-bound method of\n")
	fmt.Fprintf(b, "// impl, keyed by wire method name. A module contributes each OpSet to the\n")
	fmt.Fprintf(b, "// opsapi Slot/BindingSlot/LocalSlot; LocalBackend and RemoteBackend then share\n")
	fmt.Fprintf(b, "// the SAME wire envelopes, so remote dispatch is correct for every op shape.\n")
	fmt.Fprintf(b, "func Operations(impl %s.%s) map[string]%s.OpSet {\n", apiName, ifaceName, opsName)
	fmt.Fprintf(b, "\treturn map[string]%s.OpSet{\n", opsName)
	for _, m := range methods {
		if !m.Bound {
			continue
		}
		writeOpSet(b, m, opsName)
	}
	b.WriteString("\t}\n}\n\n")

	writeRouteBindings(b, methods, opsName)
}

// writeRouteBindings emits the IMPL-FREE `func RouteBindings() []opsapi.RouteBinding`
// — the static route + HTTP↔wire binding for each bound method, WITHOUT the LocalOp
// (which needs a provider impl). A remote-only front door (cmd/gateway-svc), which
// hosts no module and has no service to bind a LocalOp to, builds its route table
// from this and dispatches each op over a RemoteBackend to the owning peer. The
// Operation/OpBinding literals are byte-identical to the Operations(impl) entries,
// so both tables describe the same route — one source of truth.
func writeRouteBindings(b *strings.Builder, methods []method, opsName string) {
	fmt.Fprintf(b, "// RouteBindings returns the impl-free route table for each HTTP-bound method:\n")
	fmt.Fprintf(b, "// the Operation (route/auth/success) + its OpBinding (Decode/NewResp/Encode),\n")
	fmt.Fprintf(b, "// with NO LocalOp. A remote-only front door (cmd/gateway-svc) builds its route\n")
	fmt.Fprintf(b, "// table from this and dispatches each op over the edge — no provider impl needed.\n")
	fmt.Fprintf(b, "func RouteBindings() []%s.RouteBinding {\n", opsName)
	fmt.Fprintf(b, "\treturn []%s.RouteBinding{\n", opsName)
	for _, m := range methods {
		if !m.Bound {
			continue
		}
		lc := lowerFirst(m.Name)
		fmt.Fprintf(b, "\t\t{\n")
		fmt.Fprintf(b, "\t\t\tOperation: %s.Operation{Method: Method%s, Verb: %q, Path: %q, Auth: %s.%s, Success: %s},\n",
			opsName, m.Name, m.Verb, m.Path, opsName, m.AuthName, m.Success)
		fmt.Fprintf(b, "\t\t\tBinding: %s.OpBinding{Method: Method%s, Decode: decode%s, NewResp: func() any { return &%sResponse{} }, Encode: encode%s},\n",
			opsName, m.Name, m.Name, lc, m.Name)
		fmt.Fprintf(b, "\t\t},\n")
	}
	b.WriteString("\t}\n}\n")
}

// writeDecode emits decode<Method>: build the wire request envelope from the HTTP
// body (body args) and matched path wildcards (path args).
func writeDecode(b *strings.Builder, m method, opsName string) {
	lc := lowerFirst(m.Name)
	hasBody := false
	for _, a := range m.Args {
		if !a.FromPath {
			hasBody = true
		}
	}
	fmt.Fprintf(b, "// decode%s builds the %s wire request envelope from the HTTP body and path.\n", m.Name, m.Wire)
	fmt.Fprintf(b, "func decode%s(body []byte, path map[string]string) (any, error) {\n", m.Name)
	fmt.Fprintf(b, "\tvar req %sRequest\n", lc)
	if hasBody {
		b.WriteString("\tif len(body) > 0 {\n")
		b.WriteString("\t\tif err := json.Unmarshal(body, &req); err != nil {\n")
		fmt.Fprintf(b, "\t\t\treturn nil, &%s.Error{Status: %s.StatusInvalid, Msg: \"invalid json\"}\n", opsName, opsName)
		b.WriteString("\t\t}\n\t}\n")
	}
	for _, a := range m.Args {
		if a.FromPath {
			fmt.Fprintf(b, "\treq.%s = path[%q]\n", a.GoName, a.Wildcard)
		}
	}
	b.WriteString("\treturn &req, nil\n}\n\n")
}

// writeEncode emits encode<Method>: reduce the wire response envelope to the
// external HTTP body (the single domain return marshaled bare, or none) + Status.
func writeEncode(b *strings.Builder, m method, opsName string) {
	lc := lowerFirst(m.Name)
	fmt.Fprintf(b, "// encode%s reduces the %s wire response envelope to the external HTTP body + Status.\n", m.Name, m.Wire)
	fmt.Fprintf(b, "func encode%s(resp any) ([]byte, %s.Status, error) {\n", m.Name, opsName)
	fmt.Fprintf(b, "\tr := resp.(*%sResponse)\n", lc)
	fmt.Fprintf(b, "\tif r.Status != %s.StatusOK {\n", opsName)
	fmt.Fprintf(b, "\t\treturn nil, r.Status, &%s.Error{Status: r.Status, Msg: r.Err}\n", opsName)
	b.WriteString("\t}\n")
	if len(m.Rets) == 1 {
		fmt.Fprintf(b, "\tbody, err := json.Marshal(r.%s)\n", m.Rets[0].GoName)
		fmt.Fprintf(b, "\treturn body, %s.StatusOK, err\n", opsName)
	} else {
		fmt.Fprintf(b, "\treturn nil, %s.StatusOK, nil\n", opsName)
	}
	b.WriteString("}\n\n")
}

// writeOpSet emits one `MethodX: { Operation, Binding, Local }` entry of the
// Operations map: the static route (from the HTTPBind) + the binding funcs + the
// in-process invoker (unpack request envelope -> typed call -> pack response
// envelope, mirroring the server adapter minus the marshal).
func writeOpSet(b *strings.Builder, m method, opsName string) {
	lc := lowerFirst(m.Name)
	fmt.Fprintf(b, "\t\tMethod%s: {\n", m.Name)
	fmt.Fprintf(b, "\t\t\tOperation: %s.Operation{Method: Method%s, Verb: %q, Path: %q, Auth: %s.%s, Success: %s},\n",
		opsName, m.Name, m.Verb, m.Path, opsName, m.AuthName, m.Success)
	fmt.Fprintf(b, "\t\t\tBinding: %s.OpBinding{Method: Method%s, Decode: decode%s, NewResp: func() any { return &%sResponse{} }, Encode: encode%s},\n",
		opsName, m.Name, m.Name, lc, m.Name)
	fmt.Fprintf(b, "\t\t\tLocal: %s.LocalOp{Method: Method%s, Invoke: func(ctx context.Context, req, resp any) error {\n", opsName, m.Name)
	if len(m.Args) > 0 {
		fmt.Fprintf(b, "\t\t\t\trq := req.(*%sRequest)\n", lc)
	}
	var lhs []string
	for i := range m.Rets {
		lhs = append(lhs, fmt.Sprintf("r%d", i))
	}
	lhs = append(lhs, "err")
	var callArgs []string
	for _, a := range m.Args {
		callArgs = append(callArgs, "rq."+a.GoName)
	}
	callArgList := ""
	if len(callArgs) > 0 {
		callArgList = ", " + strings.Join(callArgs, ", ")
	}
	fmt.Fprintf(b, "\t\t\t\t%s := impl.%s(ctx%s)\n", strings.Join(lhs, ", "), m.Name, callArgList)
	fmt.Fprintf(b, "\t\t\t\tout := resp.(*%sResponse)\n", lc)
	b.WriteString("\t\t\t\tif err != nil {\n")
	fmt.Fprintf(b, "\t\t\t\t\tout.Status = %s.StatusOf(err)\n", opsName)
	b.WriteString("\t\t\t\t\tout.Err = err.Error()\n")
	b.WriteString("\t\t\t\t} else {\n")
	fmt.Fprintf(b, "\t\t\t\t\tout.Status = %s.StatusOK\n", opsName)
	for i, r := range m.Rets {
		fmt.Fprintf(b, "\t\t\t\t\tout.%s = r%d\n", r.GoName, i)
	}
	b.WriteString("\t\t\t\t}\n")
	b.WriteString("\t\t\t\treturn nil\n")
	b.WriteString("\t\t\t}},\n")
	b.WriteString("\t\t},\n")
}

// buildMethod validates fn's signature and reduces it to a method model, using
// qual to render (and collect imports for) each param/result type. bind, when
// non-nil, is the method's HTTP binding: it marks the method Bound, sources each
// param from the path or body, and overrides body JSON keys — so the wire request
// envelope matches the external HTTP body shape.
func buildMethod(fn *types.Func, prefix string, qual types.Qualifier, bind *httpBind) (method, error) {
	sig, ok := fn.Type().(*types.Signature)
	if !ok {
		return method{}, fmt.Errorf("not a function")
	}
	params := sig.Params()
	results := sig.Results()

	if params.Len() == 0 || !isContextContext(params.At(0).Type()) {
		return method{}, fmt.Errorf("first parameter must be context.Context")
	}
	if results.Len() == 0 || !isError(results.At(results.Len()-1).Type()) {
		return method{}, fmt.Errorf("last result must be error")
	}

	m := method{Name: fn.Name(), Wire: prefix + "." + lowerFirst(fn.Name())}
	if bind != nil {
		m.Bound = true
		m.Verb, m.Path, m.AuthName, m.Success = bind.Verb, bind.Path, bind.AuthName, bind.Success
	}

	for i := 1; i < params.Len(); i++ {
		p := params.At(i)
		if err := supported(p.Type()); err != nil {
			return method{}, fmt.Errorf("parameter %d (%s): %w", i, p.Name(), err)
		}
		f := mkField(p.Name(), i-1, "A", p.Type(), qual)
		f.Param = p.Name()
		if bind != nil {
			if wildcard, ok := bind.PathArgs[p.Name()]; ok {
				// A path arg keeps its param-name JSON tag (wire-internal only) and is
				// read from the wildcard, never the body.
				f.FromPath, f.Wildcard = true, wildcard
			} else if ext, ok := bind.BodyName[p.Name()]; ok {
				// A body arg whose external key differs from the param name: the wire
				// envelope tag becomes the external key, so a plain json.Unmarshal of the
				// (unchanged) HTTP body populates it and a RemoteBackend re-marshal is
				// identical.
				f.JSON = ext
			}
		}
		m.Args = append(m.Args, f)
	}
	for i := 0; i < results.Len()-1; i++ {
		r := results.At(i)
		if err := supported(r.Type()); err != nil {
			return method{}, fmt.Errorf("result %d: %w", i, err)
		}
		m.Rets = append(m.Rets, mkField(r.Name(), i, "R", r.Type(), qual))
	}
	// An HTTP-bound op must return at most one value besides error: the external
	// body is that value marshaled bare (single struct/slice), keeping the HTTP
	// contract unambiguous. Return several fields via a named struct instead.
	if m.Bound && len(m.Rets) > 1 {
		return method{}, fmt.Errorf("HTTP-bound method %s returns %d values besides error; return at most one (use a struct)", m.Name, len(m.Rets))
	}
	return m, nil
}

// mkField builds a wire field: an exported Go name + JSON tag (from the
// param/result name, or positional Ai/Ri when unnamed) and the rendered type.
func mkField(name string, idx int, posPrefix string, t types.Type, qual types.Qualifier) field {
	if name == "" || name == "_" {
		return field{
			GoName: fmt.Sprintf("%s%d", posPrefix, idx),
			JSON:   fmt.Sprintf("%s%d", strings.ToLower(posPrefix), idx),
			Type:   types.TypeString(t, qual),
		}
	}
	return field{GoName: exportName(name), JSON: name, Type: types.TypeString(t, qual)}
}

// supported reports whether t is a shape rpcgen can marshal, returning a clear
// error otherwise. Rejected: interfaces (other than the trailing error, already
// stripped), channels, funcs, and type parameters (generics). Basic types,
// structs, pointers, slices, arrays and maps of supported types pass; a named
// struct is trusted without descending into its fields.
func supported(t types.Type) error {
	switch u := t.(type) {
	case *types.Basic:
		return nil
	case *types.Named:
		if tp, ok := t.(*types.Named); ok && tp.TypeParams() != nil && tp.TypeParams().Len() > 0 {
			return fmt.Errorf("generic type %s is not supported", t)
		}
		switch u.Underlying().(type) {
		case *types.Interface:
			return fmt.Errorf("interface-typed value %s is not supported (marshal a concrete type instead)", t)
		case *types.Signature:
			return fmt.Errorf("func-typed value %s is not supported", t)
		case *types.Chan:
			return fmt.Errorf("channel-typed value %s is not supported", t)
		}
		return nil // struct/basic/etc. named types are marshalable
	case *types.Pointer:
		return supported(u.Elem())
	case *types.Slice:
		return supported(u.Elem())
	case *types.Array:
		return supported(u.Elem())
	case *types.Map:
		if err := supported(u.Key()); err != nil {
			return err
		}
		return supported(u.Elem())
	case *types.Struct:
		return nil
	case *types.Interface:
		return fmt.Errorf("interface-typed value %s is not supported (marshal a concrete type instead)", t)
	case *types.Chan:
		return fmt.Errorf("channel-typed value %s is not supported", t)
	case *types.Signature:
		return fmt.Errorf("func-typed value %s is not supported", t)
	case *types.TypeParam:
		return fmt.Errorf("generic type parameter %s is not supported", t)
	default:
		return fmt.Errorf("unsupported type %s", t)
	}
}

// writeImports emits a fixed two-group import block: stdlib first, then the
// gamebackend group, each sorted. Names are aliased only when they differ from
// the path's base, so the block is stable across runs.
func writeImports(b *strings.Builder, imports map[string]string) {
	var std, local []string
	for path := range imports {
		if isStdlib(path) {
			std = append(std, path)
		} else {
			local = append(local, path)
		}
	}
	sort.Strings(std)
	sort.Strings(local)

	b.WriteString("import (\n")
	writeImportGroup(b, std, imports)
	if len(std) > 0 && len(local) > 0 {
		b.WriteString("\n")
	}
	writeImportGroup(b, local, imports)
	b.WriteString(")\n\n")
}

func writeImportGroup(b *strings.Builder, paths []string, imports map[string]string) {
	for _, path := range paths {
		name := imports[path]
		if name == "" || name == pathBase(path) {
			fmt.Fprintf(b, "\t%q\n", path)
		} else {
			fmt.Fprintf(b, "\t%s %q\n", name, path)
		}
	}
}

func isStdlib(path string) bool {
	first := path
	if i := strings.IndexByte(path, '/'); i >= 0 {
		first = path[:i]
	}
	return !strings.Contains(first, ".")
}

func pathBase(path string) string {
	if i := strings.LastIndexByte(path, '/'); i >= 0 {
		return path[i+1:]
	}
	return path
}

// isContextContext reports whether t is context.Context.
func isContextContext(t types.Type) bool {
	named, ok := t.(*types.Named)
	if !ok {
		return false
	}
	obj := named.Obj()
	return obj.Pkg() != nil && obj.Pkg().Path() == "context" && obj.Name() == "Context"
}

// isError reports whether t is the predeclared error interface.
func isError(t types.Type) bool {
	named, ok := t.(*types.Named)
	if !ok {
		return false
	}
	return named.Obj().Pkg() == nil && named.Obj().Name() == "error"
}

func lowerFirst(s string) string {
	if s == "" {
		return s
	}
	r := []rune(s)
	r[0] = []rune(strings.ToLower(string(r[0])))[0]
	return string(r)
}

func exportName(s string) string {
	if s == "" {
		return s
	}
	r := []rune(s)
	r[0] = []rune(strings.ToUpper(string(r[0])))[0]
	return string(r)
}

// checkAgainst compares freshly generated src against the committed file at
// path, format-normalizing BOTH sides identically so the diff is stable across
// gofmt patch-version changes.
func checkAgainst(path string, src []byte) error {
	// #nosec G304 -- path is the codegen -out target, a trusted build-time flag, not user input.
	committed, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read committed %s: %w (run rpcgen to create it)", path, err)
	}
	want, err := format.Source(committed)
	if err != nil {
		return fmt.Errorf("gofmt committed %s: %w", path, err)
	}
	got, err := format.Source(src)
	if err != nil {
		return fmt.Errorf("gofmt generated source: %w", err)
	}
	if !bytes.Equal(want, got) {
		return fmt.Errorf("%s is stale: regenerate with rpcgen (generated output differs from committed file)", path)
	}
	return nil
}
