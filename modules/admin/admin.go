// Package admin serves the GameOps admin portal. It owns the look (theme +
// shell) and composes the dashboard from sections that modules CONTRIBUTE via
// the core "admin.section" slot — so a new module appears here without the admin
// being edited. It reads contributions (not module implementations).
package admin

import (
	"crypto/subtle"
	_ "embed"
	"html/template"
	"log/slog"
	"net/http"
	"os"
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
		w.Write(themeCSS)
	})
	ctx.Mux.HandleFunc("GET /admin", m.gate(m.handleDashboard))
	return nil
}

type pageData struct {
	Crumb    string
	Title    string
	Env      string
	User     userView
	Sections []sectionView
}

type sectionView struct {
	Title string
	KPIs  []adminapi.KPI
	Table *adminapi.Table
}

func (m *Module) handleDashboard(w http.ResponseWriter, r *http.Request) {
	var sections []sectionView
	for _, c := range m.ctx.Contributions(adminapi.Slot) {
		sec, ok := c.(adminapi.Section)
		if !ok {
			continue
		}
		content, err := sec.Render(r.Context())
		if err != nil {
			m.log.Error("admin section render failed", "section", sec.Title, "err", err)
			continue
		}
		sections = append(sections, sectionView{Title: sec.Title, KPIs: content.KPIs, Table: content.Table})
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := m.tmpl.Execute(w, pageData{
		Crumb: "Operations", Title: "Dashboard", Env: "Local", User: m.user, Sections: sections,
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
