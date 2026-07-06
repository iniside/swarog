# Status: Go backend parity z Quarkus-sketchem — OSIĄGNIĘTA i zweryfikowana

Branch `go-parity`. Plan: `docs/plans/2026-07-05-1851-go-parity-plan.md`. Data: 2026-07-05 20:00.

## Wynik

Backend Go (root repo) osiągnął **pełne parity** z `experiments/jvm-quarkus-sketch/`: jedna binarka, ROLES
wybiera monolit albo mikroserwisy, transport-transparent sync seamy **po natywnym QUIC** (`quic-go`, bez FFM),
broker-less async fanout, admin fan-out. **Teza planu potwierdzona: parity jest additywne — `core/` ani razu nie
dotknięty.**

## Kroki (branch `go-parity`, wszystkie zacommitowane)

| Krok | Commit | Co |
|---|---|---|
| 0 plan | a7af7bf | plan w repo |
| 1 role gating | 7c1cca1 | ROLES→hosted/needed, remote-stub, PORT, /healthz. Zero zmian core |
| 2 QUIC RPC core | 5c110bf | **MARQUEE** — `edge/`: 6 plików + `import quic-go` v0.60; 5 testów po realnym QUIC |
| 3 sync seamy | 1673bcf | ownerOf+verifySession po QUIC; interfejsy +error (awaria→503) |
| 4 async | 15544e8 | outbox+relay+**synchroniczny** sink+inbox (B1) |
| 5 admin fan-out | 8f55020 | local-closure + remote-URL |
| 6 run script | 323ee8f | run.ps1/sh + arch-lint config |
| 7 weryfikacja | (status) | build/vet/test/arch-lint/golangci + runtime monolit+split |

## Weryfikacja end-to-end (zweryfikowane na żywym systemie, Postgres `gamebackend`)

**Statyczne gate'y:** `go build/vet/test ./...` zielone (wszystkie pakiety), `go-arch-lint check` OK,
`golangci-lint` 0 issues na nowych pakietach.

**Monolit** (`ROLES` unset, :8080): 8 modułów, migracje, healthz 200. Register→postać→`/inventory/character/{id}`
→ **200 `starter_sword`** (ownerOf gałąź local + starter przez in-process bus). Ścieżka lokalna nietknięta.

**Split** (A=accounts,characters :8080 edge:9000 / B=inventory,admin :8081, remote=[accounts characters]):
- A: `edge listening [::]:9000` serwuje `accounts.verifySession` + `characters.ownerOf`.
- B: `remote stub registered — service resolves over the QUIC edge` dla accounts+characters.
- **Async fanout:** postać na A → outbox → relay POST → B sink → grant. Dowód: `holdings=starter_swordx1`,
  `inbox=1` (dedup), `A char_outbox unsent=0/2` (zdrenowane).
- **Sync po QUIC:** `GET /inventory/character/{id}` na B → **200 `starter_sword`** — wymagało `ownerOf` **i**
  `verifySession` po QUIC B→A.
- **Admin fan-out:** `/admin` na B → 200, składa Inventory (local) + Characters + Players/Identity (remote HTTP→A).
- **Awaria→503 (B2):** kill A → grant na B (verifySession po QUIC→martwe A) → *„auth service unavailable"* **503**
  (nie fałszywe 404); log B: *„session verify failed: timeout"*.

## Porównanie z Kotlinem (na tym samym reference-case)

- **QUIC:** Go `edge/` = 6 plików + `import quic-go`. Kotlin = `edge/msquic/` ~15 plików FFM (layouty ABI, upcalle,
  CallbackRegistry, use-after-free, schannel cert store). Ten sam ownerOf-seam po QUIC.
- **Przełącznik topologii:** Go = jawny `if` w `main()` (hosted/needed + remote-stub). Kotlin = profile + CDI
  `enabled` + hazard SRMSG00073.
- **Rdzeń:** Go `core/` **nietknięty** przez 7 kroków (wszystko w `edge/`, `outbox/`, `modules/remote/` + wiring
  w `main()` + poszerzenie interfejsów modułów). Kotlin rozpuścił rdzeń w CDI.
- **Deploy:** Go = jedna statyczna binarka (22MB) × N z różnym ROLES. Kotlin = fast-jar + JVM +
  `--enable-native-access` + dll.
- **Klasa bugów:** Go — wiring sprawdzany przez kompilator; runtime-bugi minimalne (2 lint-nity). Kotlin — pół
  sesji runtime-bugów niewidocznych dla buildu (SRMSG, IPv4/::1, use-after-free, empty-config, tx-context).

## Świadome cięcia / co dalej
- Dwa remote seamy (ownerOf+verifySession) przez jeden edge — B płaci remote-auth-hop per request (koszt splitu).
- Codec = `encoding/json` (msgpack swap za interfejsem `Codec` gdyby wire/alloc zaczął uwierać).
- Category-1 polish odroczony (sqlc/goose/prometheus/config-lib) — nie blokuje parity.
- Cert = in-memory self-signed (dev). Prod: prawdziwy trust model.
- Nie mergowane do master (branch `go-parity`).
