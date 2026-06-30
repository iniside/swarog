// Package admin serves the GameOps admin portal. It owns the look (theme +
// shell) and builds a navigable model from items that modules CONTRIBUTE via the
// core "admin.item" slot: items are grouped by Section into the sidebar, and each
// item opens its own page (GET /admin/{slug}). A new module appears here without
// the admin being edited. It reads contributions (not module implementations).
package admin

import (
	"context"
	"crypto/subtle"
	_ "embed"
	"html/template"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"strings"

	"gamebackend/core"
	"gamebackend/modules/admin/adminapi"
)

//go:embed theme.css
var themeCSS []byte

//go:embed admin.html.tmpl
var tmplText string

type Module struct {
	ctx   *core.Context
	log   *slog.Logger
	tmpl  *template.Template
	user  userView
	authU string
	authP string
}

func (*Module) Name() string        { return "admin" }
func (*Module) DependsOn() []string { return nil }

func (m *Module) Init(ctx *core.Context) error {
	m.ctx = ctx
	m.log = ctx.Log
	m.tmpl = template.Must(template.New("admin").Parse(tmplText))

	m.authU = os.Getenv("ADMIN_USER")
	m.authP = os.Getenv("ADMIN_PASS")
	m.user = newUser(m.authU)
	if m.authU == "" {
		ctx.Log.Warn("admin portal is UNAUTHENTICATED — set ADMIN_USER/ADMIN_PASS; intended for local use only")
	}

	ctx.Mux.HandleFunc("GET /admin/theme.css", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/css; charset=utf-8")
		if _, err := w.Write(themeCSS); err != nil {
			m.log.Error("theme.css write failed", "err", err)
		}
	})
	ctx.Mux.HandleFunc("GET /admin", m.gate(m.handleIndex))
	ctx.Mux.HandleFunc("GET /admin/{slug}", m.gate(m.handleItem))
	return nil
}

type pageData struct {
	Crumb, Title, Env string
	User              userView
	Groups            []navGroup
	Page              *pageView
}

type navGroup struct {
	Section string
	Items   []navItem
}

type navItem struct {
	Label, Slug string
	Active      bool
}

type pageView struct {
	Title, Err string
	KPIs       []adminapi.KPI
	Table      *adminapi.Table
}

type resolvedItem struct {
	section, label, slug string
	render               func(ctx context.Context) (adminapi.Content, error)
}

// slugify lowercases s, keeps [a-z0-9], maps space/-/_ to "-", drops other runes,
// and trims leading/trailing "-".
func slugify(s string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(s) {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9':
			b.WriteRune(r)
		case r == ' ' || r == '-' || r == '_':
			b.WriteByte('-')
		}
	}
	return strings.Trim(b.String(), "-")
}

// items resolves the contributed admin items into ordered resolvedItems with
// unique slugs (first-seen order preserved; collisions get a -2, -3, … suffix).
func (m *Module) items() []resolvedItem {
	seen := map[string]bool{}
	var out []resolvedItem
	for _, c := range m.ctx.Contributions(adminapi.Slot) {
		it, ok := c.(adminapi.Item)
		if !ok {
			continue
		}
		base := slugify(it.Label)
		if base == "" {
			base = "item"
		}
		slug := base
		for n := 2; seen[slug]; n++ {
			slug = base + "-" + strconv.Itoa(n)
		}
		seen[slug] = true
		out = append(out, resolvedItem{it.Section, it.Label, slug, it.Render})
	}
	return out
}

// buildGroups groups items by Section preserving first-seen Section order,
// marking the item matching activeSlug.
func (m *Module) buildGroups(items []resolvedItem, activeSlug string) []navGroup {
	var groups []navGroup
	idx := map[string]int{}
	for _, it := range items {
		i, ok := idx[it.section]
		if !ok {
			i = len(groups)
			idx[it.section] = i
			groups = append(groups, navGroup{Section: it.section})
		}
		groups[i].Items = append(groups[i].Items, navItem{
			Label: it.label, Slug: it.slug, Active: it.slug == activeSlug,
		})
	}
	return groups
}

func (m *Module) handleIndex(w http.ResponseWriter, r *http.Request) {
	items := m.items()
	if len(items) == 0 {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		if err := m.tmpl.Execute(w, pageData{
			Crumb: "Admin", Title: "Admin", Env: "Local", User: m.user, Groups: nil, Page: nil,
		}); err != nil {
			m.log.Error("admin render failed", "err", err)
		}
		return
	}
	http.Redirect(w, r, "/admin/"+items[0].slug, http.StatusFound)
}

func (m *Module) handleItem(w http.ResponseWriter, r *http.Request) {
	slug := r.PathValue("slug")
	items := m.items()
	var cur *resolvedItem
	for i := range items {
		if items[i].slug == slug {
			cur = &items[i]
			break
		}
	}
	if cur == nil {
		http.NotFound(w, r)
		return
	}

	var page *pageView
	content, err := cur.render(r.Context())
	if err != nil {
		m.log.Error("admin item render failed", "item", cur.label, "err", err)
		page = &pageView{Title: cur.label, Err: "failed to load: " + err.Error()}
	} else {
		page = &pageView{Title: cur.label, KPIs: content.KPIs, Table: content.Table}
	}

	groups := m.buildGroups(items, slug)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := m.tmpl.Execute(w, pageData{
		Crumb: cur.section, Title: cur.label, Env: "Local", User: m.user, Groups: groups, Page: page,
	}); err != nil {
		m.log.Error("admin render failed", "err", err)
	}
}

// gate applies HTTP Basic auth when ADMIN_USER is configured; otherwise open
// (with the startup warning) for local use.
func (m *Module) gate(next http.HandlerFunc) http.HandlerFunc {
	if m.authU == "" {
		return next
	}
	return func(w http.ResponseWriter, r *http.Request) {
		u, p, ok := r.BasicAuth()
		if !ok ||
			subtle.ConstantTimeCompare([]byte(u), []byte(m.authU)) != 1 ||
			subtle.ConstantTimeCompare([]byte(p), []byte(m.authP)) != 1 {
			w.Header().Set("WWW-Authenticate", `Basic realm="admin"`)
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

type userView struct {
	Name     string
	Initials string
}

func newUser(name string) userView {
	if name == "" {
		return userView{Name: "Local Admin", Initials: "LA"}
	}
	ini := strings.ToUpper(name)
	if len(ini) > 2 {
		ini = ini[:2]
	}
	return userView{Name: name, Initials: ini}
}
