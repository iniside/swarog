package edge

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"fmt"
	"log/slog"
	"math/big"
	"net"
	"os"
	"strings"
	"sync"
	"time"
)

// alpnProto is the ALPN protocol id negotiated for every edge connection. QUIC
// requires ALPN, and it must match on both sides (N4) — ServerMTLS (server) and
// ClientMTLS (client) both advertise exactly this.
const alpnProto = "edge"

// DevCA is the local trust anchor for the edge hop's MUTUAL TLS. Every edge
// process in a deployment shares ONE DevCA; at boot each mints its own short-
// lived leaf signed by it (a server leaf carries server-auth EKU + loopback
// SANs, a client leaf carries client-auth EKU). Because both sides trust the
// same CA, a server ACCEPTS a stream only from a client presenting a CA-signed
// cert (tls.RequireAndVerifyClientCert) and a client verifies the server against
// the same anchor (RootCAs, no InsecureSkipVerify). This closes the pre-mTLS
// impersonation hole: reaching a backend's QUIC port no longer suffices to call
// it — you must hold a CA-signed client cert.
//
// The split shares the CA by pointing every process at the SAME cert+key files
// (EDGE_CA_CERT / EDGE_CA_KEY); run.sh/run.ps1 mint one CA and export those paths
// to characters-svc, inventory-svc and gateway-svc. When the env is unset a fresh
// ephemeral CA is generated with a loud warning — fine for a single-process test
// or the loopback unit tests, but NOT shared with peers, so a real split without
// the env fails the handshake safely (never impersonates).
type DevCA struct {
	cert    *x509.Certificate
	key     *ecdsa.PrivateKey
	certDER []byte
	pool    *x509.CertPool
}

// newSerial returns a random 128-bit certificate serial number.
func newSerial() (*big.Int, error) {
	return rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
}

// GenerateDevCA mints a fresh in-memory CA (ECDSA P-256). Every call is a NEW,
// independent anchor — so a generated CA only authenticates peers that share
// THIS instance (same process, or a test that hands it to both sides). A cross-
// process split must instead share one CA on disk via LoadDevCA / DevCAFromEnv.
func GenerateDevCA() (*DevCA, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}
	serial, err := newSerial()
	if err != nil {
		return nil, err
	}
	tmpl := &x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: "gamebackend-edge-dev-ca"},
		NotBefore:             time.Now().Add(-time.Minute),
		NotAfter:              time.Now().Add(365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageCRLSign | x509.KeyUsageDigitalSignature,
		BasicConstraintsValid: true,
		IsCA:                  true,
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		return nil, err
	}
	return newDevCA(der, key)
}

// newDevCA parses a CA cert DER and pairs it with its signing key, building the
// single-cert trust pool used as both RootCAs (client) and ClientCAs (server).
func newDevCA(der []byte, key *ecdsa.PrivateKey) (*DevCA, error) {
	cert, err := x509.ParseCertificate(der)
	if err != nil {
		return nil, fmt.Errorf("edge: parse CA cert: %w", err)
	}
	pool := x509.NewCertPool()
	pool.AddCert(cert)
	return &DevCA{cert: cert, key: key, certDER: der, pool: pool}, nil
}

// LoadDevCA loads a CA cert+key from PEM files (the shared-anchor path). The cert
// must be a PEM "CERTIFICATE" and the key a PEM "EC PRIVATE KEY" (the format
// WritePEM / tools/edgeca produce).
func LoadDevCA(certPath, keyPath string) (*DevCA, error) {
	certPEM, err := os.ReadFile(certPath) //nolint:gosec // dev CA path from trusted env/config, not user input
	if err != nil {
		return nil, fmt.Errorf("edge: read CA cert %q: %w", certPath, err)
	}
	keyPEM, err := os.ReadFile(keyPath) //nolint:gosec // dev CA path from trusted env/config, not user input
	if err != nil {
		return nil, fmt.Errorf("edge: read CA key %q: %w", keyPath, err)
	}
	certBlock, _ := pem.Decode(certPEM)
	if certBlock == nil || certBlock.Type != "CERTIFICATE" {
		return nil, fmt.Errorf("edge: CA cert %q is not a PEM CERTIFICATE", certPath)
	}
	keyBlock, _ := pem.Decode(keyPEM)
	if keyBlock == nil || keyBlock.Type != "EC PRIVATE KEY" {
		return nil, fmt.Errorf("edge: CA key %q is not a PEM EC PRIVATE KEY", keyPath)
	}
	key, err := x509.ParseECPrivateKey(keyBlock.Bytes)
	if err != nil {
		return nil, fmt.Errorf("edge: parse CA key %q: %w", keyPath, err)
	}
	return newDevCA(certBlock.Bytes, key)
}

// WritePEM serializes the CA cert and key to PEM files so every edge process can
// LoadDevCA the same anchor. Used by tools/edgeca (invoked by run.sh/run.ps1). The
// key is written 0600 — it is a dev CA, but its signing key still mints trusted
// client certs, so it is not world-readable.
func (ca *DevCA) WritePEM(certPath, keyPath string) error {
	certPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: ca.certDER})
	keyDER, err := x509.MarshalECPrivateKey(ca.key)
	if err != nil {
		return fmt.Errorf("edge: marshal CA key: %w", err)
	}
	keyPEM := pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER})
	if err := os.WriteFile(certPath, certPEM, 0o644); err != nil { //nolint:gosec // CA cert is a public trust anchor, world-readable by design (the key beside it is 0600)
		return fmt.Errorf("edge: write CA cert %q: %w", certPath, err)
	}
	if err := os.WriteFile(keyPath, keyPEM, 0o600); err != nil {
		return fmt.Errorf("edge: write CA key %q: %w", keyPath, err)
	}
	return nil
}

// leaf mints a fresh short-lived leaf cert signed by the CA, for either the
// server (server-auth EKU + loopback SANs so a client dialing localhost /
// 127.0.0.1 / ::1 verifies the hostname) or the client (client-auth EKU, no
// SANs — a client is authenticated by chaining to the CA, not by name).
func (ca *DevCA) leaf(eku x509.ExtKeyUsage, server bool) (tls.Certificate, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return tls.Certificate{}, err
	}
	serial, err := newSerial()
	if err != nil {
		return tls.Certificate{}, err
	}
	tmpl := &x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: "gamebackend-edge-leaf"},
		NotBefore:             time.Now().Add(-time.Minute),
		NotAfter:              time.Now().Add(30 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{eku},
		BasicConstraintsValid: true,
	}
	if server {
		// All edge dials in this repo are loopback; cover every form a peer might
		// use as the QUIC ServerName (localhost / 127.0.0.1 / ::1).
		tmpl.DNSNames = []string{"localhost"}
		tmpl.IPAddresses = []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback}
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, ca.cert, &key.PublicKey, ca.key)
	if err != nil {
		return tls.Certificate{}, err
	}
	// Include the CA cert in the presented chain so the peer can build the path.
	return tls.Certificate{Certificate: [][]byte{der, ca.certDER}, PrivateKey: key}, nil
}

// ServerTLS builds the edge SERVER's mutual-TLS config: it presents a CA-signed
// server leaf, and — the security-critical part — REQUIRES and VERIFIES a client
// certificate that chains to this CA (tls.RequireAndVerifyClientCert + ClientCAs).
// A client with no cert, or one signed by a different CA, is rejected at the TLS
// handshake before any stream is dispatched.
func (ca *DevCA) ServerTLS() (*tls.Config, error) {
	leaf, err := ca.leaf(x509.ExtKeyUsageServerAuth, true)
	if err != nil {
		return nil, err
	}
	return &tls.Config{
		Certificates: []tls.Certificate{leaf},
		ClientAuth:   tls.RequireAndVerifyClientCert,
		ClientCAs:    ca.pool,
		NextProtos:   []string{alpnProto},
		MinVersion:   tls.VersionTLS13,
	}, nil
}

// ClientTLS builds the edge CLIENT's mutual-TLS config: it presents a CA-signed
// client leaf AND verifies the server against the same CA (RootCAs, NO
// InsecureSkipVerify). The client both proves itself to the server and rejects a
// server whose cert does not chain to the shared anchor.
func (ca *DevCA) ClientTLS() (*tls.Config, error) {
	leaf, err := ca.leaf(x509.ExtKeyUsageClientAuth, false)
	if err != nil {
		return nil, err
	}
	return &tls.Config{
		Certificates: []tls.Certificate{leaf},
		RootCAs:      ca.pool,
		NextProtos:   []string{alpnProto},
		MinVersion:   tls.VersionTLS13,
	}, nil
}

// DevCAFromEnv resolves the process's edge trust anchor. When EDGE_CA_CERT and
// EDGE_CA_KEY both point at files it loads that shared CA; otherwise it generates
// an ephemeral one and logs a LOUD warning (mirroring the ACCOUNTS_DEV_AUTH /
// ADMIN_USER dev gates) — the generated anchor is NOT shared with peers, so a
// real split without the env will fail the handshake rather than run unauthenticated.
func DevCAFromEnv(log *slog.Logger) (*DevCA, error) {
	if log == nil {
		log = slog.Default()
	}
	certPath := strings.TrimSpace(os.Getenv("EDGE_CA_CERT"))
	keyPath := strings.TrimSpace(os.Getenv("EDGE_CA_KEY"))
	if certPath != "" && keyPath != "" {
		return LoadDevCA(certPath, keyPath)
	}
	log.Warn("EDGE MUTUAL TLS using a GENERATED dev CA — dev only; this anchor is NOT shared with peers, so a real split will REJECT cross-process calls. Set EDGE_CA_CERT and EDGE_CA_KEY to a shared CA (run.sh/run.ps1 do this).")
	return GenerateDevCA()
}

// sharedCA memoizes the process's edge trust anchor so the server config and every
// client dial in the SAME process use ONE CA (their leaves chain to a common root).
// Resolved once from the env (or generated once), it is the seam that lets the
// client wrappers (remote.edgeConn, gateway.RoutedBackend) mint a CA-signed leaf
// without every call site threading a *DevCA through.
var (
	sharedCAOnce sync.Once
	sharedCA     *DevCA
	sharedCAErr  error
)

// SharedDevCA returns the process-wide edge CA, resolving it once via DevCAFromEnv
// (log is used only on that first call, for the generated-CA warning).
func SharedDevCA(log *slog.Logger) (*DevCA, error) {
	sharedCAOnce.Do(func() { sharedCA, sharedCAErr = DevCAFromEnv(log) })
	return sharedCA, sharedCAErr
}

// ServerMTLS returns an edge SERVER mutual-TLS config built from the process's
// shared CA (SharedDevCA). It replaces the old server-auth-only SelfSignedTLS.
func ServerMTLS() (*tls.Config, error) {
	ca, err := SharedDevCA(nil)
	if err != nil {
		return nil, err
	}
	return ca.ServerTLS()
}

// ClientMTLS returns an edge CLIENT mutual-TLS config built from the process's
// shared CA (SharedDevCA). It replaces the old InsecureSkipVerify ClientTLS: the
// client now presents a CA-signed cert AND verifies the server against the CA.
func ClientMTLS() (*tls.Config, error) {
	ca, err := SharedDevCA(nil)
	if err != nil {
		return nil, err
	}
	return ca.ClientTLS()
}
