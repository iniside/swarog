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
	"errors"
	"html/template"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"strings"

	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
)

//go:embed theme.css
var themeCSS []byte

//go:embed admin.html.tmpl
var tmplText string

type Module struct {
	ctx   *lifecycle.Context
	log   *slog.Logger
	tmpl  *template.Template
	user  userView
	authU string
	authP string
}

func (*Module) Name() string       { return "admin" }
func (*Module) Requires() []string { return nil }

func (m *Module) Init(ctx *lifecycle.Context) error {
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
	ctx.Mux.HandleFunc("POST /admin/{slug}", m.gate(m.handleItemPost))
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
	Form       *adminapi.Form
}

// resolvedItem is one sidebar entry ready to render. Exactly one of render /
// remote is set:
//   - LOCAL: render is the module's in-process closure, called lazily at page render.
//   - REMOTE: remote holds the already-fetched Content (or a fetch error → error
//     card). Its Section/Label were learned from the same fetch, so the nav is
//     complete before the page is rendered.
type resolvedItem struct {
	id, section, label, slug string
	render                   func(ctx context.Context) (adminapi.Content, error)
	remote                   *remoteResult
}

// remoteResult is the outcome of a remote item's RemoteFetch (an edge hop under
// the hood): on success content is populated; on a transport error err is set and
// the page shows an "unavailable" error card (a down peer never blanks /admin).
type remoteResult struct {
	content adminapi.Content
	err     error
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
//
// A LOCAL item (RemoteFetch nil) uses its own Section/Label and keeps the render
// closure for lazy in-process rendering. A REMOTE item (RemoteFetch set) is fetched
// now over the edge — Section/Label/Content all come from the peer's ItemData; an
// ErrItemAbsent (the peer has no admin surface) drops the item silently, any other
// failure keeps it as an error card (Label falls back to ID). Fetching per request
// is acceptable: /admin is low-traffic.
func (m *Module) items(ctx context.Context) []resolvedItem {
	seen := map[string]bool{}
	var out []resolvedItem
	for _, c := range m.ctx.Contributions(adminapi.Slot) {
		it, ok := c.(adminapi.Item)
		if !ok {
			continue
		}

		ri := resolvedItem{id: it.ID}
		if it.RemoteFetch == nil {
			// LOCAL — in-process closure, metadata carried on the Item.
			ri.section, ri.label, ri.render = it.Section, it.Label, it.Render
		} else {
			// REMOTE — fetch ItemData (Section/Label/Content) over the edge in one hop.
			data, err := it.RemoteFetch(ctx)
			switch {
			case errors.Is(err, adminapi.ErrItemAbsent):
				continue // no admin surface on the peer → skip silently
			case err != nil:
				m.log.Error("remote admin item unavailable", "item", it.ID, "err", err)
				ri.section, ri.label = it.ID, it.ID // no metadata to trust — key off ID
				ri.remote = &remoteResult{err: err}
			default:
				ri.section, ri.label = data.Section, data.Label
				ri.remote = &remoteResult{content: data.Content}
			}
		}

		base := slugify(ri.label)
		if base == "" {
			base = "item"
		}
		slug := base
		for n := 2; seen[slug]; n++ {
			slug = base + "-" + strconv.Itoa(n)
		}
		seen[slug] = true
		ri.slug = slug
		out = append(out, ri)
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
	items := m.items(r.Context())
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
	items := m.items(r.Context())
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
	switch {
	case cur.remote != nil:
		// REMOTE — content (or the fetch error) was resolved in items().
		if cur.remote.err != nil {
			page = &pageView{Title: cur.label, Err: "unavailable: " + cur.remote.err.Error()}
		} else {
			page = &pageView{Title: cur.label, KPIs: cur.remote.content.KPIs, Table: cur.remote.content.Table}
		}
	case cur.render != nil:
		// LOCAL — call the contributed closure in-process at render time.
		content, err := cur.render(r.Context())
		if err != nil {
			m.log.Error("admin item render failed", "item", cur.label, "err", err)
			page = &pageView{Title: cur.label, Err: "failed to load: " + err.Error()}
		} else {
			if content.Form != nil {
				content.Form.Action = "/admin/" + slug
			}
			page = &pageView{Title: cur.label, KPIs: content.KPIs, Table: content.Table, Form: content.Form}
		}
	default:
		// Neither a closure nor a remote result (a metadata-only local item).
		page = &pageView{Title: cur.label}
	}

	groups := m.buildGroups(items, slug)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := m.tmpl.Execute(w, pageData{
		Crumb: cur.section, Title: cur.label, Env: "Local", User: m.user, Groups: groups, Page: page,
	}); err != nil {
		m.log.Error("admin render failed", "err", err)
	}
}

// handleItemPost applies an editable Form's Submit for a LOCAL item. It resolves
// the item, obtains its Content via the (idempotent) render closure to reach the
// Form, parses the posted fields, and invokes Submit in-process. Success redirects
// (303) back to the GET so fresh values render; a Submit/render error re-renders the
// page with an error card. Remote and non-form items are 405 (not editable).
func (m *Module) handleItemPost(w http.ResponseWriter, r *http.Request) {
	slug := r.PathValue("slug")
	items := m.items(r.Context())
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

	// Only LOCAL items with a render closure can be edited.
	if cur.render == nil {
		http.Error(w, "not editable", http.StatusMethodNotAllowed)
		return
	}

	renderError := func(msg string) {
		page := &pageView{Title: cur.label, Err: msg}
		groups := m.buildGroups(items, slug)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		if err := m.tmpl.Execute(w, pageData{
			Crumb: cur.section, Title: cur.label, Env: "Local", User: m.user, Groups: groups, Page: page,
		}); err != nil {
			m.log.Error("admin render failed", "err", err)
		}
	}

	content, err := cur.render(r.Context())
	if err != nil {
		m.log.Error("admin item render failed", "item", cur.label, "err", err)
		renderError("failed to load: " + err.Error())
		return
	}
	if content.Form == nil || content.Form.Submit == nil {
		http.Error(w, "not editable", http.StatusMethodNotAllowed)
		return
	}

	if err := r.ParseForm(); err != nil {
		renderError("save failed: " + err.Error())
		return
	}
	values := map[string]string{}
	for _, f := range content.Form.Fields {
		values[f.Name] = r.PostFormValue(f.Name)
	}

	if err := content.Form.Submit(r.Context(), values); err != nil {
		m.log.Error("admin item submit failed", "item", cur.label, "err", err)
		renderError("save failed: " + err.Error())
		return
	}
	http.Redirect(w, r, "/admin/"+slug, http.StatusSeeOther)
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
