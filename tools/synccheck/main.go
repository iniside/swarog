// Command synccheck is a bespoke, whole-module static analyzer — sibling of
// tools/topiccheck — that will flag one seam-misuse the enforcement net cannot
// catch: an async gamebackend/bus.Emit/Publish whose side effect is then
// synchronously awaited by polling (fire the event, then busy-wait in a
// for{ sleep; read } loop for its effect). CLAUDE.md hard-constraint #7 forbids
// exactly this ("Publish never blocks and returns nothing… that's a service
// interface's job").
//
// How it works: for every function body AND every nested func literal body in the
// module, build a per-body control-flow graph (golang.org/x/tools/go/cfg), locate
// each gamebackend/bus.Emit/Publish call, and flag it when a loop body that is
// forward-reachable from the Emit contains BOTH a sleep/backoff (time.Sleep /
// time.After / a *time.Ticker|Timer.C receive) and a DB/store read (database/sql
// or pgx Query/QueryRow/Exec, or a configured store package). An Emit that is
// itself lexically inside the candidate loop is a periodic PUBLISHER, not a poll,
// and is spared by the emit-in-loop guard (this is what keeps config/listen.go
// silent).
//
// Deliberate blind spots (documented, not bugs — same honesty as topiccheck's
// string-topic note):
//   - Cross-body join is OUT OF SCOPE. go/cfg does not descend into a *ast.GoStmt
//     or *ast.FuncLit body, so an Emit in an outer body whose poll lives in a
//     spawned `go func(){ for{sleep;read} }()` lands in a disjoint CFG and is NOT
//     joined. Each body is analyzed independently.
//   - The read predicate is scoped to DB/store reads ONLY. An in-memory projection
//     getter poll is intentionally NOT matched — broadening to "any method call"
//     would fire on nearly every loop and destroy precision.
//
// Why a whole-module driver and not a go/analysis singlechecker+Facts: the same
// rationale as topiccheck — resolving gamebackend/bus.Emit/Publish/On call sites
// against shared go/types objects across the whole module (not per-package Facts)
// is the simplest way to identify "this call is THE bus", and the eventual CFG
// walk needs full type info + syntax anyway.
//
// Advisory by default (prints findings, exits 0). With --strict it exits
// non-zero when any non-allowlisted finding exists, for use as a CI gate.
//
// Test files are DELIBERATELY out of scope: Config.Mode below does not set
// packages.NeedForTest / Config.Tests. This is intentional, not an oversight —
// the emit-then-poll idiom appears only in test files today
// (inventory_test.go, config_test.go) as a legitimate test-synchronization
// pattern (assert the async path really is async), not production debt. There
// is deliberately no --include-tests flag: packages.Config{Tests: true} yields
// up to 4 package variants per directory (external test package, internal test
// binary, etc.), which would need dedup work this tool does not do. A future
// reader who wants test coverage must add that dedup, not just flip the flag.
//
// Allowlist: put a `//synccheck:allow` directive comment (optionally with
// reason="...") in the comment group immediately above the flagged statement or
// enclosing function.
package main

import (
	"flag"
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"os"
	"regexp"
	"sort"
	"strings"

	"golang.org/x/tools/go/cfg"
	"golang.org/x/tools/go/packages"
)

const busPkgPath = "gamebackend/bus"

// loadMode is the packages.Load mode: names + full type info + syntax for every
// package in the module and its deps. Deliberately omits packages.NeedForTest —
// see the package doc comment: test files are out of scope by design.
const loadMode = packages.NeedName |
	packages.NeedTypes |
	packages.NeedTypesInfo |
	packages.NeedSyntax |
	packages.NeedDeps |
	packages.NeedFiles

// Finding is one flagged emit-then-poll site. Kind distinguishes finding
// varieties (only "emit-then-poll" exists so far, but the field is kept for
// forward extensibility as the detector grows).
type Finding struct {
	Pos  token.Position
	Kind string
	Msg  string
}

// config is main's parsed flags, threaded into analyze so the testable core
// never touches flag.
type config struct {
	strict bool
}

func main() {
	strict := flag.Bool("strict", false, "exit non-zero if any non-allowlisted finding exists")
	flag.Parse()

	patterns := flag.Args()
	if len(patterns) == 0 {
		patterns = []string{"./..."}
	}

	// Tests is deliberately left unset (false): see the package doc comment.
	pkgs, err := packages.Load(&packages.Config{Mode: loadMode}, patterns...)
	if err != nil {
		fmt.Fprintf(os.Stderr, "synccheck: load: %v\n", err)
		os.Exit(2)
	}
	if packages.PrintErrors(pkgs) > 0 {
		os.Exit(2)
	}

	cfg := config{strict: *strict}
	findings := analyze(pkgs, cfg)
	for _, f := range findings {
		fmt.Printf("%s: %s: %s\n", f.Pos, f.Kind, f.Msg)
	}
	if len(findings) == 0 {
		fmt.Println("synccheck: no emit-then-poll findings (or all allowlisted)")
	}
	if cfg.strict && len(findings) > 0 {
		os.Exit(1)
	}
}

// analyze runs the emit-then-poll detector over every loaded package and returns
// the findings deterministically. It is the testable core: given loaded packages
// it returns findings without touching flag/os. It walks every *ast.FuncDecl and
// *ast.FuncLit body (each gets its own CFG — go/cfg does not descend into nested
// bodies), and flags an Emit/Publish whose effect is awaited by a reachable
// sleep+read loop.
func analyze(pkgs []*packages.Package, _ config) []Finding {
	var findings []Finding
	for _, pkg := range pkgs {
		if pkg.TypesInfo == nil {
			continue
		}
		for _, file := range pkg.Syntax {
			ast.Inspect(file, func(n ast.Node) bool {
				switch fn := n.(type) {
				case *ast.FuncDecl:
					if fn.Body != nil {
						findings = append(findings, detectBody(pkg, file, fn.Body, fn.Doc)...)
					}
				case *ast.FuncLit:
					findings = append(findings, detectBody(pkg, file, fn.Body, nil)...)
				}
				return true // descend so nested func literals get their own CFG
			})
		}
	}
	sort.Slice(findings, func(i, j int) bool {
		return findings[i].Pos.String() < findings[j].Pos.String()
	})
	return dedup(findings)
}

// dedup drops findings sharing a source position (an Emit can only be reported
// once even if two loop bodies qualify).
func dedup(in []Finding) []Finding {
	if len(in) == 0 {
		return nil
	}
	out := in[:0:0]
	seen := map[string]bool{}
	for _, f := range in {
		if seen[f.Pos.String()] {
			continue
		}
		seen[f.Pos.String()] = true
		out = append(out, f)
	}
	return out
}

// detectBody builds the CFG for one function/closure body and reports every
// Emit/Publish in it whose effect is awaited by a forward-reachable sleep+read
// loop. doc is the enclosing FuncDecl's doc comment (nil for a func literal),
// consulted by the allowlist.
func detectBody(pkg *packages.Package, file *ast.File, body *ast.BlockStmt, doc *ast.CommentGroup) []Finding {
	if body == nil {
		return nil
	}
	graph := cfg.New(body, mayReturnFn(pkg))

	// Locate every Emit/Publish call and the block it lives in. Scanning stops at
	// nested func-literal boundaries: those bodies own their own CFG, so an Emit
	// inside a closure is not attributed to this outer body.
	type emitSite struct {
		call  *ast.CallExpr
		block *cfg.Block
	}
	var emits []emitSite
	for _, block := range graph.Blocks {
		for _, node := range block.Nodes {
			walkNoFuncLit(node, func(x ast.Node) {
				call, ok := x.(*ast.CallExpr)
				if !ok {
					return
				}
				if isBusFunc(pkg, call, "Emit") || isBusFunc(pkg, call, "Publish") {
					emits = append(emits, emitSite{call: call, block: block})
				}
			})
		}
	}
	if len(emits) == 0 {
		return nil
	}

	var out []Finding
	for _, e := range emits {
		reachable := forwardReachable(e.block)
		for _, block := range graph.Blocks {
			if !reachable[block] {
				continue
			}
			if block.Kind != cfg.KindForBody && block.Kind != cfg.KindRangeBody {
				continue
			}
			loopBody := loopBodyOf(block.Stmt)
			if loopBody == nil {
				continue
			}
			// Emit-in-loop guard: an Emit lexically inside the candidate loop body
			// is a periodic publisher, not a poll of the emit's effect.
			if within(loopBody, e.call.Pos()) {
				continue
			}
			if !blockHasSleep(pkg, block) || !blockHasDBRead(pkg, block) {
				continue
			}
			if _, ok := allowlisted(pkg, file, block.Stmt.Pos(), doc); ok {
				continue
			}
			out = append(out, Finding{
				Pos:  pkg.Fset.Position(e.call.Pos()),
				Kind: "emit-then-poll",
				Msg:  emitMsg(pkg, e.call),
			})
			break // one finding per Emit site
		}
	}
	return out
}

// emitMsg builds the finding message, naming the emitted EventType var when the
// call carries one (bus.Emit(b, XEvent, v)).
func emitMsg(pkg *packages.Package, call *ast.CallExpr) string {
	ev := "an event"
	if len(call.Args) >= 2 {
		if o := objectOf(pkg, call.Args[1]); o != nil {
			ev = o.Name()
		}
	}
	return fmt.Sprintf("bus emit of %s is followed by a sleep+read poll loop; "+
		"await the event's effect via a service interface, not by polling (CLAUDE.md #7)", ev)
}

// forwardReachable returns the set of live blocks reachable from start (inclusive)
// by following successor edges — the blocks whose code runs after the Emit.
func forwardReachable(start *cfg.Block) map[*cfg.Block]bool {
	seen := map[*cfg.Block]bool{}
	queue := []*cfg.Block{start}
	for len(queue) > 0 {
		b := queue[len(queue)-1]
		queue = queue[:len(queue)-1]
		if b == nil || seen[b] || !b.Live {
			continue
		}
		seen[b] = true
		queue = append(queue, b.Succs...)
	}
	return seen
}

// loopBodyOf returns the body block statement of a for/range loop, or nil.
func loopBodyOf(stmt ast.Stmt) *ast.BlockStmt {
	switch s := stmt.(type) {
	case *ast.ForStmt:
		return s.Body
	case *ast.RangeStmt:
		return s.Body
	}
	return nil
}

// within reports whether pos falls inside the block statement's source extent.
func within(body *ast.BlockStmt, pos token.Pos) bool {
	return body.Pos() <= pos && pos <= body.End()
}

// blockHasSleep reports whether any node in block (excluding nested closures) is a
// sleep/backoff: time.Sleep, time.After, time.Tick, or a *time.Ticker|Timer.C
// channel receive.
func blockHasSleep(pkg *packages.Package, block *cfg.Block) bool {
	found := false
	for _, node := range block.Nodes {
		walkNoFuncLit(node, func(x ast.Node) {
			if found {
				return
			}
			switch e := x.(type) {
			case *ast.CallExpr:
				if fn := funcOf(pkg, e.Fun); fn != nil && fn.Pkg() != nil && fn.Pkg().Path() == "time" {
					switch fn.Name() {
					case "Sleep", "After", "Tick":
						found = true
					}
				}
			case *ast.UnaryExpr:
				if e.Op == token.ARROW && isTimerChanRecv(pkg, e.X) {
					found = true
				}
			}
		})
		if found {
			break
		}
	}
	return found
}

// isTimerChanRecv reports whether e is `t.C` where t is a *time.Ticker or
// *time.Timer (the operand of a <-t.C receive).
func isTimerChanRecv(pkg *packages.Package, e ast.Expr) bool {
	sel, ok := e.(*ast.SelectorExpr)
	if !ok || sel.Sel.Name != "C" {
		return false
	}
	named := namedOf(pkg.TypesInfo.TypeOf(sel.X))
	if named == nil || named.Obj().Pkg() == nil {
		return false
	}
	if named.Obj().Pkg().Path() != "time" {
		return false
	}
	return named.Obj().Name() == "Ticker" || named.Obj().Name() == "Timer"
}

// blockHasDBRead reports whether any node in block (excluding nested closures) is
// a DB/store read call.
func blockHasDBRead(pkg *packages.Package, block *cfg.Block) bool {
	found := false
	for _, node := range block.Nodes {
		walkNoFuncLit(node, func(x ast.Node) {
			if found {
				return
			}
			if call, ok := x.(*ast.CallExpr); ok && isDBRead(pkg, call) {
				found = true
			}
		})
		if found {
			break
		}
	}
	return found
}

// readMethods are the receiver method names treated as a DB/store read/poll.
var readMethods = map[string]bool{
	"Query":           true,
	"QueryRow":        true,
	"QueryContext":    true,
	"QueryRowContext": true,
	"Exec":            true,
	"ExecContext":     true,
}

// storeReadPkgPrefixes is the configurable extension point (empty by default):
// receiver-package path prefixes whose read methods also count as a poll, for
// module store/repo packages. Left empty so only true DB clients match and
// in-memory getters never do — see the package doc's blind-spot note.
var storeReadPkgPrefixes []string

// isDBRead reports whether call is a read method on a database/sql or pgx type
// (or a configured store package): the "read" half of a poll.
func isDBRead(pkg *packages.Package, call *ast.CallExpr) bool {
	fn := funcOf(pkg, call.Fun)
	if fn == nil || !readMethods[fn.Name()] {
		return false
	}
	sig, ok := fn.Type().(*types.Signature)
	if !ok || sig.Recv() == nil {
		return false
	}
	named := namedOf(sig.Recv().Type())
	if named == nil || named.Obj().Pkg() == nil {
		return false
	}
	return isDBReadPkg(named.Obj().Pkg().Path())
}

// isDBReadPkg reports whether path is a database client package (database/sql or
// the pgx family) or a configured store package prefix.
func isDBReadPkg(path string) bool {
	if path == "database/sql" || strings.HasPrefix(path, "github.com/jackc/pgx") {
		return true
	}
	for _, p := range storeReadPkgPrefixes {
		if p != "" && strings.HasPrefix(path, p) {
			return true
		}
	}
	return false
}

// funcOf resolves a call's function expression to its *types.Func (unwrapping a
// generic instantiation), or nil for builtins/values.
func funcOf(pkg *packages.Package, e ast.Expr) *types.Func {
	switch x := e.(type) {
	case *ast.IndexExpr:
		e = x.X
	case *ast.IndexListExpr:
		e = x.X
	}
	var id *ast.Ident
	switch x := e.(type) {
	case *ast.SelectorExpr:
		id = x.Sel
	case *ast.Ident:
		id = x
	default:
		return nil
	}
	fn, _ := pkg.TypesInfo.Uses[id].(*types.Func)
	return fn
}

// namedOf returns the *types.Named underlying t, dereferencing one pointer level,
// or nil.
func namedOf(t types.Type) *types.Named {
	if t == nil {
		return nil
	}
	if ptr, ok := t.(*types.Pointer); ok {
		t = ptr.Elem()
	}
	named, _ := t.(*types.Named)
	return named
}

// mayReturnFn builds the go/cfg mayReturn predicate: false for calls that never
// return (panic / os.Exit / log.Fatal-family), so the builder prunes infeasible
// edges after them.
func mayReturnFn(pkg *packages.Package) func(*ast.CallExpr) bool {
	return func(call *ast.CallExpr) bool {
		if id, ok := call.Fun.(*ast.Ident); ok {
			if _, isBuiltin := pkg.TypesInfo.Uses[id].(*types.Builtin); isBuiltin && id.Name == "panic" {
				return false
			}
		}
		fn := funcOf(pkg, call.Fun)
		if fn == nil || fn.Pkg() == nil {
			return true
		}
		switch fn.Pkg().Path() {
		case "os":
			if fn.Name() == "Exit" {
				return false
			}
		case "log":
			switch fn.Name() {
			case "Fatal", "Fatalf", "Fatalln":
				return false
			}
		}
		return true
	}
}

// walkNoFuncLit visits node and its descendants, calling visit for each, but does
// NOT descend into nested *ast.FuncLit bodies — those are analyzed as their own
// CFG, so a call inside a closure is never attributed to the enclosing body.
func walkNoFuncLit(root ast.Node, visit func(ast.Node)) {
	ast.Inspect(root, func(n ast.Node) bool {
		if n == nil {
			return false
		}
		if _, ok := n.(*ast.FuncLit); ok && n != root {
			return false
		}
		visit(n)
		return true
	})
}

// isBusFunc reports whether call invokes gamebackend/bus.<name>, unwrapping the
// generic instantiation (bus.Emit[T] / bus.On[T] / bus.Define[T]) that wraps the
// selector in an IndexExpr / IndexListExpr. Lifted verbatim from topiccheck.
func isBusFunc(pkg *packages.Package, call *ast.CallExpr, name string) bool {
	fun := call.Fun
	switch f := fun.(type) {
	case *ast.IndexExpr:
		fun = f.X
	case *ast.IndexListExpr:
		fun = f.X
	}
	var id *ast.Ident
	switch f := fun.(type) {
	case *ast.SelectorExpr:
		id = f.Sel
	case *ast.Ident:
		id = f
	default:
		return false
	}
	fn, ok := pkg.TypesInfo.Uses[id].(*types.Func)
	if !ok || fn.Pkg() == nil {
		return false
	}
	return fn.Pkg().Path() == busPkgPath && fn.Name() == name
}

// objectOf resolves an identifier or selector expression to the types.Object it
// refers to. Lifted verbatim from topiccheck.
func objectOf(pkg *packages.Package, e ast.Expr) types.Object {
	switch x := e.(type) {
	case *ast.Ident:
		return pkg.TypesInfo.Uses[x]
	case *ast.SelectorExpr:
		return pkg.TypesInfo.Uses[x.Sel]
	}
	return nil
}

var (
	allowRe  = regexp.MustCompile(`//\s*synccheck:allow`)
	reasonRe = regexp.MustCompile(`reason="([^"]*)"`)
)

// allowlisted reports whether the anchor position pos (a flagged statement or
// its enclosing function/loop) carries a `//synccheck:allow` directive,
// returning any reason="..." text. Generalized from topiccheck's var-decl-only
// allowlisted: pos anchors anywhere (not only a *ast.ValueSpec), and callers pass
// whatever doc comment groups apply at that site (e.g. a FuncDecl.Doc, a
// GenDecl.Doc) as docs. Comment sources checked, in order:
//  1. each *ast.CommentGroup in docs (a declaration's own .Doc, if any),
//  2. a line-adjacency fallback: any free-floating comment in file whose last
//     line is immediately above pos's line.
func allowlisted(pkg *packages.Package, file *ast.File, pos token.Pos, docs ...*ast.CommentGroup) (string, bool) {
	line := pkg.Fset.Position(pos).Line

	groups := make([]*ast.CommentGroup, 0, len(docs)+1)
	for _, cg := range docs {
		if cg != nil {
			groups = append(groups, cg)
		}
	}
	for _, cg := range file.Comments {
		if pkg.Fset.Position(cg.End()).Line == line-1 {
			groups = append(groups, cg)
		}
	}

	for _, cg := range groups {
		for _, c := range cg.List {
			if allowRe.MatchString(c.Text) {
				reason := ""
				if m := reasonRe.FindStringSubmatch(c.Text); m != nil {
					reason = m[1]
				}
				return reason, true
			}
		}
	}
	return "", false
}
