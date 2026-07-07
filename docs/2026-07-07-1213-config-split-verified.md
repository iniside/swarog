# Verified: config live-reload works in the microservices split

**Date:** 2026-07-07 12:13
**Plan:** `docs/plans/2026-07-07-1141-config-split-fix-plan.md`
**Repeatable proof:** `scripts/smoke-split-config.sh` (exits non-zero on any failed assertion)

This captures the verification I owed after the original config module shipped
monolith-only. It is NOT an eyeball claim — the split proof is a committed script
plus the raw output below.

## What was fixed

The config module was hosted only in `cmd/server`, so in the microservices split
`inventory-svc` (a config consumer) had no config and silently fell back to constants.
Fix: host config in every binary that hosts a config consumer (`cmd/inventory-svc`),
hard-require it (fail-loud), and drive `config.changed` from a DB trigger so any
writer — app, another service, or raw psql — propagates over the shared Postgres via
`LISTEN/NOTIFY`.

## Gate: `./verify.ps1 --all` → VERIFY OK

build / vet / golangci-lint / go-arch-lint / test / govulncheck all PASS.

## Split smoke: `scripts/smoke-split-config.sh` → EXIT 0

Boots characters-svc (:8080) + inventory-svc (:8081) + gateway-svc (:8082), then:

```
PASS: inventory-svc booted WITH config hosted
PASS: baseline (empty config) grants fallback 'starter_sword'
--- editing inventory:starter_item=health_potion at http://localhost:8081/admin ---
PASS: admin edit POST accepted (303)
PASS: in-process split reload (admin edit) — fresh character granted 'health_potion'
--- raw psql UPDATE inventory:starter_item=coin (bypasses all app code) ---
PASS: cross-connection reload (raw psql write via DB trigger NOTIFY) — fresh character granted 'coin'

=== SMOKE PASSED: config live-reload works in the microservices split ===
```

What each assertion proves:
1. **fail-loud satisfied** — inventory-svc boots only because config is now hosted in
   it (`module ready module=config` in `run/inventory.out.log`); with hard-require and
   config absent it would refuse to boot.
2. **baseline** — an empty config table grants the code-default `starter_sword` (the
   per-key default for an absent key — not a no-config fallback).
3. **in-process split reload** — editing `inventory:starter_item` at inventory-svc's
   own `/admin` makes a freshly-created character grant `health_potion`. (Fresh
   characters each poll: the starter is granted once at `character.created`, so a
   character created before propagation is frozen — the loop retries fresh creates.)
4. **any-writer reload** — a raw `psql UPDATE` that never touches app code propagates
   via the DB trigger's `NOTIFY` to inventory-svc's listener; the next fresh character
   grants `coin`. This is a write from a **separate connection** reaching a listener in
   a **different process**.

## Broadcast property, shown directly

A `psql` session on one connection `LISTEN config_changed`; a raw `INSERT` on a
different connection fires the trigger. The LISTENer received:

```
Asynchronous notification "config_changed" with payload "demo:broadcast"
received from server process with PID 24676.
```

i.e. Postgres delivered the NOTIFY to a session other than the writer's — the
cross-connection/cross-process fan-out the whole design rests on, demonstrated rather
than asserted.

## Honest scope
- The split runs ONE config listener (inventory-svc), so the smoke proves
  cross-connection propagation to that listener. The many-listener broadcast is shown
  separately by the `psql LISTEN` capture above.
- Live-deleting a config key is out of scope (the trigger is INSERT/UPDATE only; the
  listener has no delete path). Absent keys use their code default; that is different
  from deleting a previously-set key at runtime.
