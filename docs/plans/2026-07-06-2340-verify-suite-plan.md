# Plan: `verify` — jednoskryptowa sieć automatycznej weryfikacji (bez CI)

> Po zatwierdzeniu zapis do repo jako `docs/plans/2026-07-06-HHMM-verify-suite-plan.md`.

## Context

Repo ma **zero automatyzacji** — brak CI (`.github/workflows` nie istnieje), brak Makefile,
brak hooków. Bramki (build/vet/test/golangci/go-arch-lint) odpalają się tylko gdy ktoś je
ręcznie wywoła. User: „ufać ci nie ufam, potrzebujemy całej tej listy na wszelki wypadek" —
czyli **sieć bezpieczeństwa niezależna ode mnie**: jeden skrypt `verify.ps1`/`verify.sh`
(bliźniaki, jak `run.ps1`/`run.sh`) odpalający całą listę narzędzi weryfikujących
poprawność/założenia, plus kod którego te narzędzia potrzebują (fuzz targets, property-testy,
analizator topiców). **CI świadomie poza zakresem** (user).

Wynik: `./verify.ps1` (albo `-Fast`/`-All`/`-Slow`/`-Strict`) → tabela PASS/FAIL/SKIP,
exit≠0 gdy padnie bramka blokująca. Guardrails łapią błędy niezależnie od tego czy ja się pomylę.

### Gating (zatwierdzone: „rozsądny podział")

| Stage | Typ | Komenda / uwaga |
|---|---|---|
| build | BLOK | `go build ./...` |
| vet | BLOK | `go vet ./...` |
| golangci-lint | BLOK | `golangci-lint run ./...` (konfig `.golangci.yml` istnieje) |
| go-arch-lint | BLOK | `go-arch-lint check` |
| test | BLOK | `go test ./...` — **property-testy (rapid), wire-contract, ORAZ seed-corpus fuzz targetów jadą tutaj** (go test odpala seedy `Fuzz*` jako zwykłe testy — deterministycznie, bez `-fuzz`) |
| test-race | ADVISORY (`-All`) | `go test ./... -race` — **TYLKO gdy probe wykryje cgo+gcc** (`go env CGO_ENABLED`==1 && `command -v gcc`); inaczej SKIP z notą. **NIE blokujące** — ta maszyna ma CGO_ENABLED=0, brak gcc (tylko MSVC clang, którego race-runtime nie napędzi) → `-race` by wywalał build. Zweryfikowane. |
| govulncheck | BLOK | `govulncheck ./...` — **tryb tekstowy** (exit 3 na vuln); `-json` ZAWSZE exit 0 → nie używać |
| fuzz | ADVISORY (`-All`/`-Slow`) | rzeczywisty `-fuzz`: grep `func Fuzz*`, per target `go test <pkg> -run '^$' -fuzz '^Name$' -fuzztime 10s`. **Poza blokującą** — fuzzing eksploruje nowe wejścia, mógłby zamigać na czerwono niezwiązanie z diffem. Regresja seedów już jest w blokującym `test`. |
| apidiff-events | ADVISORY | compat kontraktów `*events` vs **`HEAD`** (nie origin/master — nie istnieje lokalnie); **sam nie faila** → grep `Incompatible changes:` |
| topiccheck | ADVISORY | „zdefiniowany topic bez subscribera"; exit≠0 tylko z `--strict` |
| gremlins | ADVISORY (`-Slow`) | mutation na czystych pakietach; wolne; exit 0 bez progów |
| nilaway | ADVISORY (opcj.) | tylko jeśli binarka obecna (NIE auto-instalować — false-positives) |

ADVISORY = raportuje, nie wywraca exit-code, **chyba że** `-Strict`. Auto-instalacja brakujących
CLI (`govulncheck`/`apidiff`/`gremlins`) via `go install` w skrypcie (check `Get-Command`/`command -v`).

**Świadomie odroczone** (żeby nie marnować czasu): `goldie` (moje ręczne wire-contract piny
już to robią), `testcontainers` (pamięć: lokalny Postgres = test DB), `go-deadlock` (wymaga
podmiany typu mutexa — inwazyjne), `jsonschema` (pokrywa się z golden pinami). Wymienione w
planie, nie budowane.

---

## Step 1 — fuzz + property-testy dla `edge`  `[opus]` (think)

**(a) Co:** nowe pliki w `package edge`: `edge/fuzz_test.go`, `edge/prop_test.go`. Dodaje dep
`pgregory.net/rapid` (`go get pgregory.net/rapid && go mod tidy`).
**(b) Dlaczego teraz:** pierwszy, bo (i) dodaje dep rapid którego Step 2 też używa (kolejność
go.mod), (ii) fuzz/property to najwyższy-leverage guard na granicę niezaufanego wejścia (edge decode).
**(c) Jak** (wszystko `package edge` — typy `readFrame`/`request`/`dispatch`/`longestPrefix`
nieeksportowane, testy MUSZĄ być wewnętrzne; istniejące `edge_test.go`/`router_test.go` też są `package edge`):
- **Fuzz** (`edge/fuzz_test.go`): `FuzzReadFrame` (dowolne bajty → `readFrame(bytes.NewReader)` nie panikuje;
  guard `n>maxFrameSize` przed `make` już jest w `frame.go:37-39` — test pilnuje że zostaje),
  `FuzzFrameRoundTrip` (write→read == identyczne, seed cap ~kilka KB nie 16 MiB),
  `FuzzCodecDecodeRequest` (`defaultCodec.Decode(bajty, &request)` nie panikuje),
  `FuzzDispatch` (skonstruowany `NewServer()`+`Handle`/`HandlePrefix`, `srv.dispatch(bajty)` nigdy nie
  panikuje, zwraca dobrze uformowany `response`: `OK==true`⇒pusty Error, `OK==false`⇒nil Payload, i
  `codec.Encode(resp)` się udaje). Seed corpus przez `f.Add` per target.
- **PROAKTYWNY FIX (nie warunkowy):** `dispatch` robi `Decode` PRZED instalacją `defer recover()`
  (`server.go:190` vs `:196`), a `serveStream` też nie ma recover i biegnie w goroutine (`wg.Go`) →
  panika w `Decode` (możliwa przy custom `Codec`, np. przyszły msgpack) **crashuje cały proces**.
  Przenieść `defer func(){ recover() }()` na sam początek `dispatch` (przed `Decode`) — jedna linia.
  NIE gatować tego na fuzz (`FuzzDispatch` używa panic-safe encoding/json, więc **strukturalnie tego
  nie wykryje**). To realny latentny bug, robimy od razu.
- **Property (rapid)** (`edge/prop_test.go`): (1) codec round-trip — `Custom`/`Make[T]` konkretnego
  structa, `Decode(Encode(v))==v`; (2) frame round-trip — `rapid.SliceOfN(rapid.Byte(),0,8<<10)`
  (cap ~8 KB, nie 1 MiB — rapid robi ~100 przebiegów, duże slice'y = wolny stage);
  (3) prefix longest-match — losowy zbiór distinct prefixów + losowa metoda, oracle-loop liczy
  oczekiwany najdłuższy, porównaj z `longestPrefix`; exact-wins przez `dispatch`-level test.
  API: `rapid.Check(t, func(t *rapid.T){ x := gen.Draw(t,"label"); ... })`.
**Weryfikacja:** `go test ./edge/... -race`; każdy fuzz `-fuzztime=10s` zielony.

## Step 2 — property-testy dla `outbox` + `registry`  `[sonnet]` (think)

**(a) Co:** `outbox/relay_prop_test.go` (`package outbox`), `registry/registry_prop_test.go`
(`package registry_test` — API eksportowane, black-box OK).
**(b) Dlaczego teraz:** po Step 1 (rapid już w go.mod). Niezależne pakiety, brak konfliktu z edge.
**(c) Jak** (spec z researchu):
- **outbox `deliver`** (`relay.go`, sygn. `deliver(pending []outRow, post func(url,eventID string,payload []byte) error) []int64` — pure, bez DB/HTTP): generuj rosnący batch `outRow` + `subscribers map[string][]string` + wzorzec porażek (prealokowany `[]bool` per kolejność wywołań). Asercje z docstringa relay: per-subscriber stop-on-first-failure (po porażce URL X dla id i, żaden id j>i nie POSTowany do X), all-or-nothing per row, rows bez subscribera zawsze sent, `sent` to rosnący podciąg. Plus `ParseSubscribers` round-trip (`topic=url1,url2;...`).
- **registry `Provide`/`Require`**: N distinct name→value, `Require` zwraca provided (deep-equal); `Require` na brak → panic zawiera „required service"; podwójny `Provide` → panic „already provided". Jedna funkcja property per konkretny `T` (generyki rapid: T fixed per test).
**Weryfikacja:** `go test ./outbox/... ./registry/... -race`.

## Step 3 — analizator `topiccheck` (zdefiniowany topic bez subscribera)  `[opus]` (think)

**(a) Co:** nowy `tools/topiccheck/` (`main.go` + logika + `analyzer_test.go`). Dodaje dep
`golang.org/x/tools`. Komponent arch-lint. Komentarz-allowlist nad `PlayerRegisteredEvent`.
**(b) Dlaczego teraz:** niezależne od 1/2 (inny dep x/tools); przed skryptem (Step 4 go wywołuje).
**(c) Jak** (kluczowy insight z researchu — **NIE** `singlechecker`/Facts: fakty płyną downstream,
tu potrzeba upstream):
- Bespoke driver: `go/packages.Load({Mode: NeedTypes|NeedTypesInfo|NeedSyntax|NeedDeps}, "./...")`
  raz na cały moduł. Dwa przebiegi po każdym `*packages.Package` (inspector + `TypesInfo`):
  - **Define**: `*ast.CallExpr` gdzie callee (`TypesInfo.Uses[sel.Sel].(*types.Func)`) ma
    `Pkg().Path()=="gamebackend/bus"` i `Name()=="Define"` → wejdź do `ValueSpec` (CallExpr→enclosing
    var wymaga `inspector.WithStack` albo śledzenia rodzica — nie samego `Preorder`), weź
    `Defs[ident]` (`*types.Var` np. `CreatedEvent`), wyciągnij literał topicu z `Types[call.Args[0]].Value`.
    Klucz mapy: `pkgPath+"."+ident` (stabilny, czytelny). `defined[key]={topic,pos}`.
  - **On**: callee `bus.On` → drugi arg (`et`), rozwiąż `Uses` jego identyfikatora do tego samego
    `*types.Var` (obiekty współdzielone w jednym `packages.Load`) → `subscribed[key]=true`.
- Diff: `defined \ subscribed \ allowlist` → report. Allowlist: komentarz
  `//topiccheck:allow-unsubscribed reason="..."` bezpośrednio nad `var` (czyt. `ast.CommentGroup`).
  Dodać ten komentarz nad `PlayerRegisteredEvent` (`accountsevents.go:18` — znany zamierzony brak
  subscribera, CLAUDE.md:88). Exit≠0 tylko z flagą `--strict`; domyślnie report + exit 0.
- **arch-lint** (`.go-arch-lint.yml`): `components:` `topiccheck: { in: tools/topiccheck }`;
  `deps:` `topiccheck: { mayDependOn: [] }` (pusta lista — inaczej default dałby mu bus/registry/contrib;
  vendor x/tools dozwolony globalnie przez `depOnAnyVendor: true`).
- Self-test: `analysistest` na fixture **pod `tools/topiccheck/testdata/`** (go-tooling-ignored — inaczej
  go-arch-lint zgłosi je jako unmatched package) — topic bez On → wykryty; z allowlist → pominięty.
- **Znane ograniczenie (do doc-comentu):** analizator widzi subskrypcje tylko przez `bus.On(et)` po
  identyczności `*types.Var`. Surowe `b.Subscribe("topic", …)` ze stringiem byłoby niewidoczne →
  false-positive „unsubscribed". W repo nikt tak nie robi (wszędzie `bus.On`), ale odnotować.
**Weryfikacja:** `go run ./tools/topiccheck ./...` → wypisuje 0 nieallowlistowanych (bo player.registered
w allowliście); `go-arch-lint check` zielony; `go test ./tools/...`.

## Step 4 — skrypt `verify.ps1` + `verify.sh` (bliźniaki)  `[opus]` (think)

**(a) Co:** `verify.ps1`, `verify.sh` w root. Logi do `run/verify/<stage>.log`.
**(b) Dlaczego teraz:** ostatni artefakt — wywołuje fuzz targety (Step 1) i topiccheck (Step 3).
**(c) Jak** (wzorce z `run.ps1`/`run.sh`, ale **odwrócone error-handling**: verify NIE abortuje na
pierwszej porażce — zbiera całą tabelę):
- Flagi jak w `run.*` (PS `param([switch]$Fast/$All/$Slow/$Strict)`; bash `case --fast|--all|--slow|--strict`).
  Preambuła `$PSScriptRoot` / `cd "$(dirname "$0")"` (verbatim z run.*).
- **Stage-table** jako struktura: `{Name, Blocking, ScriptBlock}`. PS: `$ErrorActionPreference='Continue'`
  w sekcji stage'ów (NIE 'Stop'), `Run-Stage` łapie `$LASTEXITCODE`/wyjątek → PASS/FAIL/SKIP do listy.
  bash: bez `set -e` w fazie run, każdy stage `if cmd; then PASS else FAIL fi`, parallel arrays
  `STAGE_NAMES/STATUS/BLOCKING` (idiom z run.sh).
- Kolejność blokujących: build→vet→golangci→go-arch-lint→`go test ./...` (bez race)→govulncheck.
  (Seedy fuzz jadą w tym `go test`. Property/wire-contract też.)
- **test-race** (advisory `-All`): probe `go env CGO_ENABLED`==1 AND `command -v gcc`/`Get-Command gcc`.
  Jest → `go test ./... -race`. Brak → **SKIP** z notą „race pominięty: brak cgo/gcc" (NIE FAIL).
  Zweryfikowane na tej maszynie: CGO=0, brak gcc → SKIP.
- **Exit-code gotchas (zaszyć):** govulncheck tryb tekstowy (nie `-json`); apidiff `-incompatible` + `[ -n "$out" ]`→FAIL (sam nie faila); gremlins bez progów = report/exit 0.
- **apidiff-events** (advisory): BASE = **`HEAD`** (ostatni commit; `origin/master` NIE istnieje lokalnie —
  `git rev-parse origin/master` pada; sens: „czy niezacommitowana zmiana zepsuła compat"). Jeśli working
  tree == HEAD (nic niezacommitowanego), stage jest no-opem — OK. `git worktree add --detach $tmp HEAD`,
  `apidiff -w snap.api <pkg>` z cwd w worktree, `apidiff -incompatible snap.api <pkg>` z cwd w root;
  pakiety: `accountsevents`, `charactersevents`, `matchevents`, `admin/adminapi`; `git worktree remove --force`
  w trap/finally. (PS: `try/finally`; bash: `trap ... EXIT`.)
- **gremlins** (`-Slow`): pętla `gremlins unleash ./<pkg>/...` po `edge gateway outbox registry bus` (scoping = pozycyjny PATH; brak klucza „exclude" w schemacie), bez progów (advisory).
- **Instalacja brakujących CLI — PINNED + ANONSOWANA** (nie ciche `@latest`): skrypt sprawdza obecność
  (`Get-Command`/`command -v`); brak → wypisuje `installing <tool>@<pinned>` i `go install` z **przypiętą
  wersją**: `govulncheck@v1.5.0`, `apidiff@<pinned-pseudo>`, `gremlins@v0.6.0`. Flaga `-NoInstall`/`--no-install`
  by pominąć (dla „trust-nobody" bez sieci → te stage'e SKIP).
- **Summary**: tabela `Name | Status | Blocking` (PS kolor Green/Red/Yellow jak run.ps1; bash plain,
  jak run.sh — zachować asymetrię bliźniaków). `exit 1` gdy jakiś FAIL i (Blocking lub `-Strict`).
**Weryfikacja:** `./verify.ps1 -Fast` zielony; `-All` uruchamia advisory; wymuszony fail (np. tag drift)
→ exit≠0 i FAIL w tabeli.

## Step 5 — integracja end-to-end + commit  `[inline]`

**(a) Co:** odpalić `verify.ps1 -All` na żywo, naprawić co wyjdzie, zaktualizować CLAUDE.md
(sekcja „Commands" — dopisać `./verify.ps1` jako jedną-komendę bramkę + install-linijki nowych narzędzi).
**(b) Dlaczego inline:** integracja wielu części + żywa pętla naprawcza; osobny kontekst nic nie da.
**(c) Jak:** uruchom pełny skrypt; dla każdej bramki potwierdź PASS; udowodnij zęby (wstrzyknij drift →
exit≠0 → rewert, jak przy wire-contract). Commit po każdym Stepie (subagenty commitują swoje):
`test(edge)`, `test(outbox,registry)`, `feat(tools): topiccheck analyzer`, `chore(verify): one-shot gate script`.

---

## Dispatch (do zatwierdzenia)

| Step | Lane | Uzasadnienie |
|---|---|---|
| 1 edge fuzz+property | `[opus]` | granica niezaufanego wejścia, prefix-router property, możliwy panic-ordering finding |
| 2 outbox+registry property | `[sonnet]` | w pełni wyspecyfikowane z researchu, pure funkcje |
| 3 topiccheck analyzer | `[opus]` | bespoke go/packages driver, types.Object matching — najtrudniejszy kod |
| 4 verify skrypt | `[opus]` | load-bearing artefakt; poprawność gating/exit-code = cały sens sieci |
| 5 integracja+commit | `[inline]` | żywa pętla naprawcza |

Effort: [opus]→think hard, [sonnet]→think (ustalone). Trailery: [opus]→Opus 4.8, [sonnet]→Sonnet 4.6.
Deps dodane: `pgregory.net/rapid` (Step 1), `golang.org/x/tools` (Step 3) — sekwencyjnie (go.mod).

## Ryzyka / decyzje
- **rapid/x/tools w go.mod produkcyjnym:** to test/tool-only deps, ale trafiają do `go.mod` głównego
  modułu (Go nie ma osobnego test-module). Akceptowalne; `go mod tidy` je utrzyma.
- **apidiff cross-platform:** logika worktree jest naturalna w bash; w PS wymaga `try/finally` +
  `git worktree`. Jeśli PS-owa wersja okaże się krucha, apidiff-stage może być bash-only z notą w PS
  („uruchom przez verify.sh") — decyzja w Step 4, nie blokuje reszty.
- **gremlins wolne:** tylko pod `-Slow`, nigdy w `-Fast`. Scoping pętlą po czystych pakietach.
- **race NIE-blokujący (rozstrzygnięte, C1):** ta maszyna (Go 1.26.1, CGO_ENABLED=0, brak gcc — tylko
  MSVC clang którego race-runtime nie napędzi) nie zbuduje `-race`. Blokujący `test` jest bez race;
  race to advisory `-All` z probe (SKIP gdy brak toolchainu). Bez tego skrypt byłby wiecznie czerwony.
- **fuzz poza blokującą (M2):** rzeczywisty `-fuzz` eksploruje nowe wejścia → mógłby migać czerwono
  niezwiązanie z diffem. Blokująca jest tylko regresja SEEDÓW (zwykły `go test` je odpala). Pełny
  `-fuzz` pod `-All`/`-Slow`.
- **Go 1.26.1** (nie 1.25 z `go 1.25.0` w go.mod — toolchain wyższy). Fuzz/rapid/x/tools kompatybilne.
