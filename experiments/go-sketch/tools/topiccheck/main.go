// Command topiccheck is a bespoke, whole-module static analyzer that flags every
// event topic DECLARED via bus.Define[T]("topic") but never SUBSCRIBED anywhere in
// the module — via the in-process bus.On(b, ...Event, h), the durable
// bus.OnTx(b, ...Event, subscriber, h), or the raw-topic durable
// bus.OnTxRaw(b, "topic", subscriber, h). A defined-but-unsubscribed topic is
// usually dead vocabulary — an event nobody reacts to — so surfacing it keeps the
// published-event surface honest as the modular monolith grows.
//
// Why a whole-module driver and not a go/analysis singlechecker+Facts: analysis
// Facts flow definer→importer, i.e. downstream. Here the relationship runs the
// WRONG way — the SUBSCRIBER (bus.On call) lives downstream of the package that
// DEFINES the topic, so a Fact exported by the definer can never observe its own
// subscribers. Instead we load the whole module in ONE packages.Load: that shares
// a single set of go/types objects across every package, so the EventType var
// object recorded at its Define site is the SAME pointer resolved at each On site,
// and a plain object-identity map diffs the two sets.
//
// Advisory by default (prints findings, exits 0). With --strict it exits non-zero
// when any non-allowlisted topic is unsubscribed, for use as a CI gate.
//
// Allowlist: put a `//topiccheck:allow-unsubscribed` directive comment (optionally
// with reason="...") in the comment group immediately above the var's declaration.
// The one known-intentional case today is accountsevents.PlayerRegisteredEvent,
// which is emitted but not yet wired into match/rating (see CLAUDE.md).
//
// LIMITATION: subscriptions are detected through three funcs. bus.On and bus.OnTx
// both pass the EventType var as arg[1], matched by OBJECT IDENTITY (the shared
// go/types object recorded at the Define site). bus.OnTxRaw passes a STRING-LITERAL
// topic as arg[1], matched by TOPIC STRING against every Define's topic literal. The
// only remaining invisible path is a raw b.Bus.Subscribe("topic", handler) with a
// string literal (audit's best-effort in-process sinks use it) — but every topic so
// consumed (config.changed, match.finished) is ALSO covered by a typed bus.On
// subscriber elsewhere, and player.registered is allowlisted, so none go red.
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
	"sort"

	"golang.org/x/tools/go/packages"
)

const busPkgPath = "gamebackend/bus"

// loadMode is the packages.Load mode: names + full type info + syntax for every
// package in the module and its deps, so Define and On sites resolve to shared
// go/types objects.
const loadMode = packages.NeedName |
	packages.NeedTypes |
	packages.NeedTypesInfo |
	packages.NeedSyntax |
	packages.NeedDeps |
	packages.NeedFiles

// Finding is one defined-but-unsubscribed topic.
type Finding struct {
	Pos   token.Position
	Topic string
}

// defined records a bus.Define site keyed by the EventType var's types.Object.
type defined struct {
	obj     types.Object
	topic   string
	pos     token.Position
	allowed bool
	reason  string
}

func main() {
	strict := flag.Bool("strict", false, "exit non-zero if any non-allowlisted topic is unsubscribed")
	flag.Parse()

	patterns := flag.Args()
	if len(patterns) == 0 {
		patterns = []string{"./..."}
	}

	pkgs, err := packages.Load(&packages.Config{Mode: loadMode}, patterns...)
	if err != nil {
		fmt.Fprintf(os.Stderr, "topiccheck: load: %v\n", err)
		os.Exit(2)
	}
	if packages.PrintErrors(pkgs) > 0 {
		os.Exit(2)
	}

	findings := analyze(pkgs)
	for _, f := range findings {
		fmt.Printf("%s: topic %q defined but never subscribed\n", f.Pos, f.Topic)
	}
	if len(findings) == 0 {
		fmt.Println("topiccheck: all defined topics are subscribed (or allowlisted)")
	}
	if *strict && len(findings) > 0 {
		os.Exit(1)
	}
}

// analyze runs the two passes (define, on) over every loaded package and diffs
// them by object identity, dropping allowlisted definitions. It is the testable
// core: given loaded packages it returns the findings deterministically.
func analyze(pkgs []*packages.Package) []Finding {
	defs := map[types.Object]*defined{}
	subscribed := map[types.Object]bool{} // EventType var objects (On, OnTx)
	subscribedTopics := map[string]bool{} // string-literal topics (OnTxRaw)

	for _, pkg := range pkgs {
		if pkg.TypesInfo == nil {
			continue
		}
		collectDefines(pkg, defs)
		collectSubscribes(pkg, subscribed, subscribedTopics)
	}

	var findings []Finding
	for obj, d := range defs {
		if subscribed[obj] || subscribedTopics[d.topic] || d.allowed {
			continue
		}
		findings = append(findings, Finding{Pos: d.pos, Topic: d.topic})
	}
	sort.Slice(findings, func(i, j int) bool {
		if findings[i].Pos.String() != findings[j].Pos.String() {
			return findings[i].Pos.String() < findings[j].Pos.String()
		}
		return findings[i].Topic < findings[j].Topic
	})
	return findings
}

// collectDefines finds every `var X = bus.Define[T]("topic")` at package level and
// records X's object, its topic literal, and whether it carries an allowlist
// directive.
func collectDefines(pkg *packages.Package, defs map[types.Object]*defined) {
	for _, file := range pkg.Syntax {
		for _, decl := range file.Decls {
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
					if i >= len(vs.Values) {
						continue
					}
					call, ok := vs.Values[i].(*ast.CallExpr)
					if !ok || !isBusFunc(pkg, call, "Define") || len(call.Args) < 1 {
						continue
					}
					topic, ok := stringConst(pkg, call.Args[0])
					if !ok {
						continue
					}
					obj := pkg.TypesInfo.Defs[name]
					if obj == nil {
						continue
					}
					d := &defined{
						obj:   obj,
						topic: topic,
						pos:   pkg.Fset.Position(name.Pos()),
					}
					if reason, ok := allowlisted(pkg, file, gd, vs); ok {
						d.allowed, d.reason = true, reason
					}
					defs[obj] = d
				}
			}
		}
	}
}

// collectSubscribes marks every subscription's target topic. bus.On and bus.OnTx
// both take the EventType var as arg[1] — matched by object identity into
// subscribed. bus.OnTxRaw takes a string-literal topic as arg[1] — matched by topic
// string into subscribedTopics (the raw-topic durable subscribe used by audit).
func collectSubscribes(pkg *packages.Package, subscribed map[types.Object]bool, subscribedTopics map[string]bool) {
	for _, file := range pkg.Syntax {
		ast.Inspect(file, func(n ast.Node) bool {
			call, ok := n.(*ast.CallExpr)
			if !ok || len(call.Args) < 2 {
				return true
			}
			switch {
			case isBusFunc(pkg, call, "On"), isBusFunc(pkg, call, "OnTx"):
				if obj := objectOf(pkg, call.Args[1]); obj != nil {
					subscribed[obj] = true
				}
			case isBusFunc(pkg, call, "OnTxRaw"):
				if topic, ok := stringConst(pkg, call.Args[1]); ok {
					subscribedTopics[topic] = true
				}
			}
			return true
		})
	}
}

// isBusFunc reports whether call invokes gamebackend/bus.<name>, unwrapping the
// generic instantiation (bus.Define[T] / bus.On[T]) that wraps the selector in an
// IndexExpr / IndexListExpr.
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
// refers to (the shared EventType var object).
func objectOf(pkg *packages.Package, e ast.Expr) types.Object {
	switch x := e.(type) {
	case *ast.Ident:
		return pkg.TypesInfo.Uses[x]
	case *ast.SelectorExpr:
		return pkg.TypesInfo.Uses[x.Sel]
	}
	return nil
}

// stringConst extracts a constant string value from an expression via type info.
func stringConst(pkg *packages.Package, e ast.Expr) (string, bool) {
	tv, ok := pkg.TypesInfo.Types[e]
	if !ok || tv.Value == nil || tv.Value.Kind() != constant.String {
		return "", false
	}
	return constant.StringVal(tv.Value), true
}

var (
	allowRe  = regexp.MustCompile(`//\s*topiccheck:allow-unsubscribed`)
	reasonRe = regexp.MustCompile(`reason="([^"]*)"`)
)

// allowlisted reports whether the var declaration carries a
// //topiccheck:allow-unsubscribed directive in the comment group immediately above
// it (checked via the GenDecl/ValueSpec doc comments plus a line-based fallback
// over the file's free-floating comments), returning any reason="..." text.
func allowlisted(pkg *packages.Package, file *ast.File, gd *ast.GenDecl, vs *ast.ValueSpec) (string, bool) {
	declLine := pkg.Fset.Position(gd.Pos()).Line

	var groups []*ast.CommentGroup
	if gd.Doc != nil {
		groups = append(groups, gd.Doc)
	}
	if vs.Doc != nil {
		groups = append(groups, vs.Doc)
	}
	for _, cg := range file.Comments {
		if pkg.Fset.Position(cg.End()).Line == declLine-1 {
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
