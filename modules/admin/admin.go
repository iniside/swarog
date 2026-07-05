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
	"encoding/json"
	"errors"
	"fmt"
	"html/template"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

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
	// http fetches remote items' ItemData over HTTP (a peer process's /admin-data/
	// <id>). Shared, with a sane timeout so a slow/down peer never blocks /admin.
	http *http.Client
}

func (*Module) Name() string       { return "admin" }
func (*Module) Requires() []string { return nil }

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.ctx = ctx
	m.log = ctx.Log
	m.tmpl = template.Must(template.New("admin").Parse(tmplText))
	m.http = &http.Client{Timeout: 3 * time.Second}

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

// remoteResult is the outcome of fetching a remote item's ItemData: on success
// content is populated; on a transport error / non-2xx / timeout err is set and
// the page shows an "unavailable" error card (a down peer never blanks /admin).
type remoteResult struct {
	content adminapi.Content
	err     error
}

// errRemoteNotFound signals a 404 from a peer's /admin-data/<id>: the remote
// module has no admin surface, so the item is skipped silently (not an error card).
var errRemoteNotFound = errors.New("remote admin item not found (404)")

// fetchRemote GETs a peer's /admin-data/<id> and decodes the ItemData. A 404 maps
// to errRemoteNotFound (skip); any other non-2xx, a transport failure, or a decode
// error is returned so the caller renders an error card.
func (m *Module) fetchRemote(ctx context.Context, url string) (adminapi.ItemData, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return adminapi.ItemData{}, err
	}
	resp, err := m.http.Do(req)
	if err != nil {
		return adminapi.ItemData{}, err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode == http.StatusNotFound {
		return adminapi.ItemData{}, errRemoteNotFound
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return adminapi.ItemData{}, fmt.Errorf("remote admin returned %s", resp.Status)
	}
	var data adminapi.ItemData
	if err := json.NewDecoder(resp.Body).Decode(&data); err != nil {
		return adminapi.ItemData{}, err
	}
	return data, nil
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
// A LOCAL item (RemoteURL empty) uses its own Section/Label and keeps the render
// closure for lazy in-process rendering. A REMOTE item (RemoteURL set) is fetched
// now — Section/Label/Content all come from the peer's ItemData; a 404 drops the
// item silently, any other failure keeps it as an error card (Label falls back to
// ID). Fetching per request is acceptable: /admin is low-traffic.
func (m *Module) items(ctx context.Context) []resolvedItem {
	seen := map[string]bool{}
	var out []resolvedItem
	for _, c := range m.ctx.Contributions(adminapi.Slot) {
		it, ok := c.(adminapi.Item)
		if !ok {
			continue
		}

		ri := resolvedItem{id: it.ID}
		if it.RemoteURL == "" {
			// LOCAL — in-process closure, metadata carried on the Item.
			ri.section, ri.label, ri.render = it.Section, it.Label, it.Render
		} else {
			// REMOTE — fetch ItemData for Section/Label/Content in one round-trip.
			data, err := m.fetchRemote(ctx, it.RemoteURL)
			switch {
			case errors.Is(err, errRemoteNotFound):
				continue // no admin surface on the peer → skip silently
			case err != nil:
				m.log.Error("remote admin item unavailable", "item", it.ID, "url", it.RemoteURL, "err", err)
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
			page = &pageView{Title: cur.label, KPIs: content.KPIs, Table: content.Table}
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
