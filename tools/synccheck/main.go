// Command synccheck is a bespoke, whole-module static analyzer — sibling of
// tools/topiccheck — that will flag one seam-misuse the enforcement net cannot
// catch: an async gamebackend/bus.Emit/Publish whose side effect is then
// synchronously awaited by polling (fire the event, then busy-wait in a
// for{ sleep; read } loop for its effect). CLAUDE.md hard-constraint #7 forbids
// exactly this ("Publish never blocks and returns nothing… that's a service
// interface's job").
//
// STEP 1 STATUS: this file is the harness only — whole-module loading, the
// exit/print contract, and the helpers the CFG-based detector will need. analyze
// does not detect anything yet; it always returns zero findings. The detector
// pass (go/cfg reachability from an Emit/Publish call to a sleep+read loop, with
// an emit-in-loop guard for legitimate periodic publishers) lands in a follow-up
// step.
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
	"go/constant"
	"go/token"
	"go/types"
	"os"
	"regexp"

	"golang.org/x/tools/go/packages"
)

//nolint:unused // reused by isBusFunc, which is reused by the Step 2 CFG detector (not yet wired in Step 1).
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

// analyze runs the detector passes over every loaded package and returns the
// findings deterministically. It is the testable core: given loaded packages
// it returns findings without touching flag/os.
//
// STEP 1 STUB: no detector is wired yet, so this always returns zero findings.
// The CFG-based emit-then-poll pass (walking every *ast.FuncDecl and *ast.FuncLit
// body, locating Emit/Publish calls via isBusFunc, and checking forward-reachable
// loop bodies for both a sleep/backoff call and a DB/store read) is added in a
// follow-up step, along with its testdata fixtures.
func analyze(pkgs []*packages.Package, _ config) []Finding {
	var findings []Finding
	for _, pkg := range pkgs {
		if pkg.TypesInfo == nil {
			continue
		}
		// Detector pass not yet wired (Step 1 stub) — nothing to do per package.
	}
	return findings
}

// isBusFunc reports whether call invokes gamebackend/bus.<name>, unwrapping the
// generic instantiation (bus.Emit[T] / bus.On[T] / bus.Define[T]) that wraps the
// selector in an IndexExpr / IndexListExpr. Lifted verbatim from topiccheck.
//
//nolint:unused // reused by the Step 2 CFG detector (not yet wired in Step 1).
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
//
//nolint:unused // reused by the Step 2 CFG detector (not yet wired in Step 1).
func objectOf(pkg *packages.Package, e ast.Expr) types.Object {
	switch x := e.(type) {
	case *ast.Ident:
		return pkg.TypesInfo.Uses[x]
	case *ast.SelectorExpr:
		return pkg.TypesInfo.Uses[x.Sel]
	}
	return nil
}

// stringConst extracts a constant string value from an expression via type
// info. Lifted verbatim from topiccheck.
//
//nolint:unused // reused by the Step 2 CFG detector (not yet wired in Step 1).
func stringConst(pkg *packages.Package, e ast.Expr) (string, bool) {
	tv, ok := pkg.TypesInfo.Types[e]
	if !ok || tv.Value == nil || tv.Value.Kind() != constant.String {
		return "", false
	}
	return constant.StringVal(tv.Value), true
}

//nolint:unused // reused by allowlisted, which is reused by the Step 2 CFG detector (not yet wired in Step 1).
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
//
//nolint:unused // reused by the Step 2 CFG detector (not yet wired in Step 1).
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
