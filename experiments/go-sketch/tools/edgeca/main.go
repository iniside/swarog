// Command edgeca mints a local dev CA (cert + key PEM) for the edge hop's mutual
// TLS and writes it to the given paths. run.sh / run.ps1 invoke it ONCE before
// launching the microservices split, then export EDGE_CA_CERT / EDGE_CA_KEY at
// those paths to every edge process so each mints its own leaf under one shared
// trust anchor. It is a dev convenience only — never part of a shipped binary.
//
//	go run ./tools/edgeca -cert run/edge-ca.crt -key run/edge-ca.key
package main

import (
	"flag"
	"fmt"
	"os"

	"gamebackend/edge"
)

func main() {
	certPath := flag.String("cert", "", "output path for the CA certificate (PEM)")
	keyPath := flag.String("key", "", "output path for the CA private key (PEM)")
	flag.Parse()

	if *certPath == "" || *keyPath == "" {
		fmt.Fprintln(os.Stderr, "edgeca: both -cert and -key are required")
		os.Exit(2)
	}

	ca, err := edge.GenerateDevCA()
	if err != nil {
		fmt.Fprintf(os.Stderr, "edgeca: generate CA: %v\n", err)
		os.Exit(1)
	}
	if err := ca.WritePEM(*certPath, *keyPath); err != nil {
		fmt.Fprintf(os.Stderr, "edgeca: write CA: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("edgeca: wrote dev CA cert=%s key=%s\n", *certPath, *keyPath)
}
