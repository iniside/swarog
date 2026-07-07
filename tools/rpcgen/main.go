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
//   - first param must be context.Context (not marshaled; the server adapter uses
//     context.Background() exactly as today's hand adapters do — NO ctx
//     propagation is claimed),
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
	"go/format"
	"go/types"
	"os"
	"path/filepath"
	"sort"
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

	if err := os.WriteFile(*out, src, 0o644); err != nil {
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

	apiPkg, iface, err := findInterface(pkgs, ifaceName)
	if err != nil {
		return nil, err
	}
	return generate(apiPkg, iface, ifaceName, prefix, outPkg)
}

// findInterface locates the named interface type across the loaded packages,
// rejecting a generic (type-parameterised) interface.
func findInterface(pkgs []*packages.Package, ifaceName string) (*types.Package, *types.Interface, error) {
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
		return pkg.Types, iface, nil
	}
	return nil, nil, fmt.Errorf("interface %q not found in %d loaded package(s)", ifaceName, len(pkgs))
}

// method is one validated interface method reduced to what codegen needs.
type method struct {
	Name string   // Go method name, e.g. "OwnerOf"
	Wire string   // wire method string, e.g. "characters.ownerOf"
	Args []field  // params after ctx, in order
	Rets []field  // results before the trailing error, in order
}

// field is one wire-envelope field: its exported Go name, JSON tag, and the Go
// source spelling of its type (with package qualifiers).
type field struct {
	GoName string
	JSON   string
	Type   string
}

// generate builds and gofmt-normalizes the glue source for iface.
func generate(apiPkg *types.Package, iface *types.Interface, ifaceName, prefix, outPkg string) ([]byte, error) {
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
	for i := 0; i < iface.NumMethods(); i++ {
		fn := iface.Method(i)
		m, err := buildMethod(fn, prefix, qual)
		if err != nil {
			return nil, fmt.Errorf("method %s: %w", fn.Name(), err)
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
	fmt.Fprintf(&b, "// one handler per method. *%s.Server satisfies it.\n", imports[edgePkgPath])
	fmt.Fprintf(&b, "type Registrar interface {\n\tHandle(method string, h %s.Handler)\n}\n\n", imports[edgePkgPath])
	fmt.Fprintf(&b, "// RegisterServer installs one edge adapter per method of impl onto reg. Each\n")
	fmt.Fprintf(&b, "// adapter unmarshals the request, calls impl with context.Background() (matching\n")
	fmt.Fprintf(&b, "// the hand-written adapters — NO ctx propagation), and marshals the response\n")
	fmt.Fprintf(&b, "// envelope, folding a returned error into Status/Err via %s.StatusOf.\n", opsName)
	fmt.Fprintf(&b, "func RegisterServer(reg Registrar, impl %s.%s) {\n", apiName, ifaceName)
	for _, m := range methods {
		writeServerAdapter(&b, m, opsName)
	}
	b.WriteString("}\n")

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
	fmt.Fprintf(b, "\treg.Handle(Method%s, func(reqPayload []byte) ([]byte, error) {\n", m.Name)
	fmt.Fprintf(b, "\t\tvar req %sRequest\n", lc)
	b.WriteString("\t\tif err := json.Unmarshal(reqPayload, &req); err != nil {\n\t\t\treturn nil, err\n\t\t}\n")

	// Call impl.
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
	fmt.Fprintf(b, "\t\t%s := impl.%s(context.Background()%s)\n", strings.Join(lhs, ", "), m.Name, callArgList)

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

// buildMethod validates fn's signature and reduces it to a method model, using
// qual to render (and collect imports for) each param/result type.
func buildMethod(fn *types.Func, prefix string, qual types.Qualifier) (method, error) {
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

	for i := 1; i < params.Len(); i++ {
		p := params.At(i)
		if err := supported(p.Type()); err != nil {
			return method{}, fmt.Errorf("parameter %d (%s): %w", i, p.Name(), err)
		}
		m.Args = append(m.Args, mkField(p.Name(), i-1, "A", p.Type(), qual))
	}
	for i := 0; i < results.Len()-1; i++ {
		r := results.At(i)
		if err := supported(r.Type()); err != nil {
			return method{}, fmt.Errorf("result %d: %w", i, err)
		}
		m.Rets = append(m.Rets, mkField(r.Name(), i, "R", r.Type(), qual))
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
