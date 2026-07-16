# weles ↔ processctl fleet parity

weles is zero-sharing: its fleet manifest (`weles/src/manifest.rs`) is a
hand-copied port of `tools/processctl/src/fleet.rs` (the Development flavor),
not an import. The BLOCKING verifyctl stage `weles-fleet-parity`
(`tools/verifyctl/src/stages/weles_fleet_parity.rs`) machine-checks that copy
against the real processctl source of truth on every `--fast` run — per
service: name/pkg, http/edge/player ports, `has_db`, `pool_max`, the full
normalized composed env (peer `*_EDGE_ADDR`/`*_HTTP_ADDR`, `DATABASE_POOL_MAX_CONNECTIONS`,
dev-seeds, `TLS_MODE`, security CIDR), and boot-order-vs-dependency-graph
consistency. The only excluded env keys are the ambient `SERVICE_ENV_ALLOWLIST`
passthrough (PATH/HOME/…); everything topology-shaped is compared. weles is not
exercised by split-proof, so this stage is its ONLY parity gate — hence blocking.

## The dev/prod seam (an M1 warning, not a today problem)

Both manifests fold three unrelated concerns into one untagged bag of env pairs
(`weles::manifest::ServiceDef::env_extra`; `processctl` `ServiceSpec::env`):

1. **Topology wiring** — `PORT`, `EDGE_ADDR`, peer `*_EDGE_ADDR`/`*_HTTP_ADDR`,
   `PLAYER_EDGE_ADDR` (structural, identical in any deployment flavor).
2. **Dev-mode seeds / opt-ins** — `ACCOUNTS_DEV_AUTH`, `APIKEYS_DEV_SEED`,
   `INVENTORY_DEV_GRANT`, `ADMIN_COOKIE_SECURE=0` (development-only; a real
   deployment must NOT ship these).
3. **A security knob** — `TRUSTED_PROXY_CIDRS` (a production concern living in
   the same bag as the dev seeds).

processctl bolts a production-ish variant on top with a post-hoc
`if flavor == FleetFlavor::Proof { … }` overlay (`tools/processctl/src/fleet.rs`),
mutating the already-composed dev env in place. When weles grows an M1 prod
flavor it must NOT copy that pattern: a post-hoc mutation of a dev baseline is
how a forgotten dev seed leaks into production. Instead, structurally separate
the three concerns — the wiring belongs to the topology, the seeds belong to a
development-only overlay that a prod flavor never applies, and the security knob
is its own deliberate input — so a prod flavor is built by OMITTING the dev
overlay, not by patching it away afterward. No prod flavor exists today and this
note deliberately does NOT add one (that would smuggle in an unbuilt seam); it
records the constraint for whoever does.

Evidence base: [weles pre-M1 backlog research](../status/2026-07-15-1815-weles-pre-m1-backlog-research-status.md).
