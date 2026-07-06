package edge

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"math/big"
	"time"
)

// alpnProto is the ALPN protocol id negotiated for every edge connection. QUIC
// requires ALPN, and it must match on both sides (N4) — SelfSignedTLS (server)
// and ClientTLS (client) both advertise exactly this.
const alpnProto = "edge"

// SelfSignedTLS builds an in-memory self-signed TLS config for the edge SERVER.
// It mints a fresh ECDSA P-256 key + certificate at call time (no files, no OS
// cert store — the deliberate contrast with the Kotlin schannel pain) valid for
// localhost, and advertises the "edge" ALPN proto.
func SelfSignedTLS() (*tls.Config, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}

	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return nil, err
	}

	tmpl := &x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: "localhost"},
		NotBefore:             time.Now().Add(-time.Minute),
		NotAfter:              time.Now().Add(365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
		DNSNames:              []string{"localhost"},
	}

	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		return nil, err
	}

	cert := tls.Certificate{
		Certificate: [][]byte{der},
		PrivateKey:  key,
	}

	return &tls.Config{
		Certificates: []tls.Certificate{cert},
		NextProtos:   []string{alpnProto},
		MinVersion:   tls.VersionTLS13,
	}, nil
}

// ClientTLS builds the TLS config for the edge CLIENT. It skips certificate
// verification (dev: the server presents an ephemeral self-signed cert) and
// advertises the same "edge" ALPN proto the server requires.
func ClientTLS() *tls.Config {
	return &tls.Config{
		InsecureSkipVerify: true, //nolint:gosec // dev edge: ephemeral self-signed server cert, ALPN-gated
		NextProtos:         []string{alpnProto},
		MinVersion:         tls.VersionTLS13,
	}
}
