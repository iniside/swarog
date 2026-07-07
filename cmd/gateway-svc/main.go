// Command gateway-svc is the player-facing front door. It hosts NO module (no
// DB, no bus, no lifecycle) and therefore does NOT use internal/app.Run: it is a
// pure transport process. On the QUIC edge it prefix-routes player calls to the
// backend that owns each method family (characters.* → characters-svc,
// inventory.* → inventory-svc); on HTTP it reverse-proxies player-facing paths to
// the backend that serves them. It imports only gateway + edge.
package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"time"

	"golang.org/x/time/rate"

	"gamebackend/edge"
	"gamebackend/gateway"
	"gamebackend/httpmw"
)

// env returns the trimmed value of key, or def when unset/blank.
func env(key, def string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return def
}

// envFloat reads key as a float64, returning def when unset or unparseable.
func envFloat(key string, def float64) float64 {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return def
	}
	f, err := strconv.ParseFloat(v, 64)
	if err != nil {
		return def
	}
	return f
}

// envInt reads key as an int, returning def when unset or unparseable.
func envInt(key string, def int) int {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return def
	}
	return n
}

// normalizeAddr accepts both ":8082" and "8082" forms and returns ":8082".
func normalizeAddr(port string) string {
	port = strings.TrimSpace(port)
	if port == "" {
		return ":8082"
	}
	if strings.HasPrefix(port, ":") {
		return port
	}
	return ":" + port
}

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))

	httpAddr := normalizeAddr(os.Getenv("PORT"))
	gatewayEdgeAddr := env("GATEWAY_EDGE_ADDR", ":9100")
	charsEdgeAddr := env("CHARACTERS_EDGE_ADDR", "localhost:9000")
	invEdgeAddr := env("INVENTORY_EDGE_ADDR", "localhost:9001")
	charsHTTP := env("CHARACTERS_HTTP_ADDR", "localhost:8080")
	invHTTP := env("INVENTORY_HTTP_ADDR", "localhost:8081")

	// One self-healing relay per backend peer, shared across all inbound conns.
	chars := gateway.NewRoutedBackend(charsEdgeAddr)
	inv := gateway.NewRoutedBackend(invEdgeAddr)

	// QUIC front door: prefix-route each method family to its owning backend.
	srv := edge.NewServer()
	srv.HandlePrefix("characters.", chars.Forward)
	srv.HandlePrefix("inventory.", inv.Forward)

	tlsConf, err := edge.SelfSignedTLS()
	if err != nil {
		log.Error("edge tls", "err", err)
		os.Exit(1)
	}
	if err := srv.ListenAddr(gatewayEdgeAddr, tlsConf); err != nil {
		log.Error("edge listen", "err", err)
		os.Exit(1)
	}
	log.Info("gateway edge listening", "addr", srv.Addr())

	// HTTP front door: /healthz here, everything else reverse-proxied to the
	// backend that serves its prefix. /admin lives in inventory-svc.
	proxy := gateway.NewHTTPProxy(map[string]string{
		"/admin":      invHTTP,
		"/characters": charsHTTP,
		"/inventory":  invHTTP,
	})
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})
	// Everything else falls through to the reverse proxy (which knows the
	// per-prefix origins). "GET /healthz" is more specific, so it still wins.
	mux.Handle("/", proxy)

	// The gateway is the player-facing front door: it ALWAYS rate limits (default
	// 20 rps, burst 40), unlike internal/app where it is opt-in. Same SkipInfra so
	// /healthz is never throttled. Honors X-Forwarded-For only from trusted proxies.
	trusted, err := httpmw.ParseCIDRs(os.Getenv("TRUSTED_PROXY_CIDRS"))
	if err != nil {
		log.Error("parse TRUSTED_PROXY_CIDRS", "err", err)
		os.Exit(1)
	}
	rps := envFloat("RATE_LIMIT_RPS", 20)
	burst := envInt("RATE_LIMIT_BURST", 40)
	lim := httpmw.NewIPLimiter(rate.Limit(rps), burst)
	handler := httpmw.RateLimit(lim, httpmw.ClientIP(trusted), httpmw.SkipInfra, mux)
	log.Info("gateway rate limiting enabled", "rps", rps, "burst", burst, "trusted_cidrs", len(trusted))

	httpSrv := &http.Server{Addr: httpAddr, Handler: handler, ReadHeaderTimeout: 10 * time.Second}
	go func() {
		log.Info("gateway http listening", "addr", httpSrv.Addr)
		if err := httpSrv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("http stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown, mirroring app.go's order: stop HTTP first (no new
	// inbound), then close the edge listener (no new relayed calls), then the
	// backend relays.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt)
	<-stop
	log.Info("shutting down")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := httpSrv.Shutdown(shutdownCtx); err != nil {
		log.Error("http shutdown", "err", err)
	}
	if err := srv.Close(); err != nil {
		log.Error("edge shutdown", "err", err)
	}
	_ = chars.Close()
	_ = inv.Close()
	log.Info("bye")
}
