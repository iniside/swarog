> **Status / errata (2026-07-13): SUPERSEDED — do not implement this design.**
> Step 8 of `2026-07-12-1214-architecture-remediation-rust-tooling-plan.md`
> replaced the proposed shipping `core/conformance` types and per-module policy
> entries with tool-owned policy in `tools/conformance`; modules expose only minimal
> factual probes with no conformance dependency or feature flag. The body below is
> retained unchanged as historical design context.

# Convention-Conformance Harness (`core/conformance` + `tools/conformance`) — Plan

> Po zatwierdzeniu: Step 0 kopiuje ten plik do `docs/plans/2026-07-12-<HHMM>-convention-conformance-harness-plan.md` (repo = source of truth; plan-mode pozwala pisać tylko tutaj).

## Context

Od 3 dni ta sama klasa poprawek wraca ping-pongiem: zachowanie utwardzone w module X, jego bliźniak w module Y zostaje nietknięty (nazwane wprost w plan-docu rundy 3: *"a fix not carried to the more-hostile path"*). Testy per-moduł z definicji tego nie łapią — brak testu w Y wygląda identycznie jak "konwencja nie dotyczy Y". Cel: testy targetujące **konwencję**, nie moduł — harness, który (a) sam enumeruje `modules/*` z dysku, (b) wymaga od KAŻDEGO modułu jawnej deklaracji wobec KAŻDEJ konwencji (`Applies(fixture)` albo `NotApplicable{why}` — cisza = czerwony test), (c) wykonuje generyczne asercje na fixture'ach.

**Feasibility potwierdzona researchem (7 subagentów, 2026-07-12):**
- Boot register+init poza `app::run` to ustalony wzorzec: `Context::with_db_and_transport(pool, NoopTransport)` + `App::build()` robią to już `tools/routecheck` (`main.rs:154-198`), `requirecheck`, `topiccheck`. `App::build()` = tylko register+init, zero portów, sync (`core/lifecycle/src/app.rs:91`).
- Pełny zestaw modułów: `server::modules(&ProcessWiring::new(), None)` (`cmd/server/src/lib.rs:28-52`) — tak konsumuje go `tools/checkmodules`.
- Drift-check dysk↔lista ma 3 precedensy: `archcheck::crate_dirs()` (`tools/archcheck/src/main.rs:570-582` — celowo filesystem, nie cargo metadata, by złapać niezarejestrowany moduł), `checkmodules` test `split_fleet_matches_cmd_dirs` (`tools/checkmodules/src/tests.rs:12-38`), `splitproof::preflight_fleet` (`tools/splitproof/src/main.rs:545-562`).
- Legalność: rules 1/2 gate'ują tylko `Kind::Module`, rule 16 tylko `Kind::Core` — `tools/` może importować moduły. **WYJĄTEK (blocker znaleziony w review): rule 3** (`tools/archcheck/src/main.rs:251-276`) skanuje KAŻDY pakiet pod kątem non-dev dep na crate `gateway`; allowlist to `FRONT_DOOR_HOSTS` + `GATEWAY_CHECKER_HOSTS = ["checkmodules","topiccheck","requirecheck"]` (`main.rs:61`). Harness musi zostać dopisany do `GATEWAY_CHECKER_HOSTS` (Step 4 to robi). Typy współdzielone żyją w `core/*` (kierunek moduł→core sankcjonowany; wzór: `opsapi::SLOT`, `edge::EDGE_SLOT`, `httpmw::LAYER_SLOT`).
- Precedens "pub test-support w crate'cie": `asyncevents::testing` (`core/asyncevents/src/lib.rs:423` — celowo `pub`, nie `#[cfg(test)]`, dla cross-crate konsumentów).

**Why not extend / depend on X** (obowiązkowa analiza nakładających się systemów):
- **routecheck** — sprawdza parity op-ów (front/serve/integrity) na wartościach ze slotów; nie ma pojęcia fixture'ów per-moduł ani macierzy konwencji. Zostaje bez zmian; conformance to komplement (zachowania, nie routing). Nie rozszerzamy, bo mieszałby dwa różne kontrakty w jednym binarium z osobnymi trybami env.
- **checkmodules** — dostarcza listy modułów per profil; **zależymy od niego** (nie duplikujemy list). Nie wciskamy harnessu w jego `#[cfg(test)]`, bo T6 wymaga mutacji env procesu — w `cargo test` (równoległe wątki jednego binarium) to wyścig; własne single-threaded binarium jest deterministyczne.
- **topiccheck/splitproof** — graf subskrypcji / live-proof; inna warstwa. Splitproof jest za ciężki (boot 13 procesów) na macierz konwencji.
- **archcheck** — tekstowo-grafowy, bez wykonywania kodu; konwencje behawioralne wymagają wykonania fixture'ów.

**Decyzja mechanizmu (zmiana wobec wstępnego szkicu ze slotem):** fixture'y NIE są kontrybuowane w `init` przez slot, tylko eksponowane jako `pub mod conformance { pub fn entry() -> conformance::Entry }` w każdym module (wzór `asyncevents::testing`). Powód: fixture T7 musi skonstruować weryfikator z zawsze-padającą zależnością (fake) — fake'i nie mogą żyć w produkcyjnym `init`. Gwarancja "zapomniałem = czerwony" zostaje zachowana inaczej i mocniej: (1) ręczna lista `entries()` w harnessie jest diffowana per-entry z `modules/*` na dysku ORAZ z nazwami z `server::modules()` (didn't-forget self-check), (2) dopisanie modułu do listy wymaga istnienia `entry()` — inaczej harness się nie kompiluje, (3) macierz kompletności: każdy moduł × każda konwencja musi mieć jawny stance.

## Docelowe typy (`core/conformance`)

```rust
// core/conformance/src/lib.rs — crate bez zależności na tokio/sqlx/moduły (przechodzi rule 16 trywialnie)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Convention { EnvValidation, InputByteCaps, InfraOutage503, ArgonParity }
impl Convention { pub const ALL: [Convention; 4] = [/* … */]; }

#[derive(Clone)]
pub struct Entry { pub module: &'static str, pub stances: Vec<(Convention, Stance)> }

#[derive(Clone)]
pub enum Stance { Applies(Fixture), NotApplicable { why: &'static str } }

#[derive(Clone)]
pub enum Fixture {
    /// T6: harness ustawia var=bad_value i oczekuje, że świeży pełny App::build() zwróci Err wymieniający var.
    EnvValidation(Vec<EnvCase>),                    // EnvCase { var: &'static str, bad_value: &'static str }
    /// T8: probe(len) -> true == odrzucone; harness asertuje !probe(cap) && probe(cap+1).
    InputByteCaps(Vec<CapCase>),                    // CapCase { name: &'static str, cap: usize, probe: Arc<dyn Fn(usize) -> bool + Send + Sync> }
    /// T7: probe() uruchamia weryfikator modułu z padającą zależnością; musi zwrócić Unavailable (503-class), nie Rejected (401-class).
    InfraOutage503(Vec<OutageCase>),                // OutageCase { name: &'static str, probe: Arc<dyn Fn() -> BoxFuture<'static, OutageClass> + Send + Sync> }
    /// T2: harness zbiera wszystkie Applies i asertuje parami równość parametrów.
    ArgonParity(ArgonParams),                       // ArgonParams { m_cost: u32, t_cost: u32, p_cost: u32, output_len: usize }
}
pub enum OutageClass { Unavailable, Rejected, Other(String) }
```

Uwaga na `BoxFuture`: żeby crate został wolny od ciężkich zależności, użyć `Pin<Box<dyn Future<Output = OutageClass> + Send>>` zdefiniowanego ręcznie (bez `futures`), albo dopuścić dev-lekki `futures-core`. Decyzja w Step 1 — preferencja: ręczny alias typu.

## Macierz stance'ów (ROZSTRZYGNIĘTA — wykonawca nie ocenia, tylko implementuje; zmiana stance'u wymaga powrotu do usera)

| Moduł | T6 EnvValidation | T8 InputByteCaps | T7 InfraOutage503 | T2 ArgonParity |
|---|---|---|---|---|
| accounts | **NA** (env accounts to presence-gates: `EPIC_CLIENT_ID`, `ACCOUNTS_DEV_AUTH` — fail-closed przez nieobecność, brak parsowania numerycznego przy init) | Applies: email 320, password 1024 (`MAX_EMAIL_BYTES`/`MAX_PASSWORD_BYTES` — `modules/accounts/src/lib.rs:50-51`; wymaga hoistu pure-fn, patrz Step 2) | Applies: probe BEZ sieci — nieskonfigurowany provider epic ⇒ `Unavailable` (wzór potwierdzony w `modules/accounts/src/tests.rs:250-252`); NIE konstruować `OidcVerifier` na martwy URL (realne I/O) | Applies (`accounts::argon2_params_for_parity_test()` już `pub` — `cmd/server/src/tests.rs:13-18`) |
| admin | NA (`ADMIN_COOKIE_SECURE`/`ADMIN_OPEN` to gate'y zachowania, nie walidacja wartości) | Applies: username 128, password 1024 (dziś inline literals w handlerze `modules/admin/src/lib.rs:749-751` — wymaga hoistu, patrz Step 2) | **NA** (admin nie ma własnego infra-weryfikatora: login to lokalne DB, a błąd remote adminData renderuje się jako błąd strony, nie klasyfikacja auth) | Applies (`admin::argon2_params_for_parity_test()` już `pub`) |
| apikeys | NA | Applies: `apikeysapi::MAX_KEY_BYTES` 256 — publiczny const + gotowe check-fns (`modules/apikeys/src/admin.rs:66`, `store.rs:95`), zero refactoru | NA (weryfikator kluczy żyje w gateway) | NA |
| gateway | NA | NA (limity requestów to httpmw, nie moduł) | Applies: `RealKeyVerifier` + `LookupUnavailable` są `pub` (`modules/gateway/src/keys.rs`); fake `UnavailableVerifier` przenieść z `tests.rs:99` do zawsze-kompilowanego `conformance.rs` (precedens `asyncevents::testing`) | NA |
| audit | Applies: `AUDIT_RETENTION_DAYS` ∈ {"0","-3"} ⇒ init Err zawierający nazwę var (potwierdzone: `modules/audit/src/lib.rs:342-348`; wartości nieparsowalne cicho defaultują — dlatego bad_value MUSI być parsowalną nie-dodatnią liczbą) | NA | NA | NA |
| scheduler | **NA** (`SCHEDULER_ENABLED` to boolean gate; interval to dane w DB, nie env) | NA | NA | NA |
| match | NA | **NA** ("ReportId has no byte-cap today; candidate for T8 adoption" — NIE dodawać nowej walidacji w tym rollout'cie) | NA | NA |
| characters, inventory, config, rating, leaderboard | NA z konkretnym `why` per moduł (dev-gates = zachowanie fail-closed przez nieobecność, nie walidacja wartości) | NA | NA | NA |

`why` ma być zdaniem sprawdzalnym przez recenzenta ("no env parsed at init beyond dev-gates, which fail closed by absence"), nie "n/d".

## Kroki

**Step 0 — plan do repo** `[inline]`
(a) Skopiować ten plik do `docs/plans/2026-07-12-<HHMM>-convention-conformance-harness-plan.md`. (b) Najpierw, bo repo jest źródłem prawdy i kolejne kroki się do niego odwołują. (c) Mechaniczne. (d) `[inline]`. Commit: `docs(plans): convention-conformance harness plan`.

**Step 1 — crate `core/conformance` (package name: `conformance`)** `[fable]` (effort: think hard)
(a) Nowe: `core/conformance/Cargo.toml` (package `conformance`), `core/conformance/src/lib.rs` (typy jak wyżej + `impl Entry { pub fn stance(&self, c: Convention) -> Option<&Stance> }`); root `Cargo.toml` members + `[workspace.dependencies]` wpis. **Nazewnictwo (kolizja rozwiązana):** core crate = `conformance`, harness = `conformancecheck` (spójne z rodziną archcheck/topiccheck/routecheck/requirecheck). (b) Pierwsze, bo wszystko dalej importuje te typy. (c) Zero zależności poza std (ręczny alias `pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>`). Doc-comment na crate: kontrakt harnessu (cisza=fail, NA wymaga why). Testy typów zbędne (czyste dane) — ale `Convention::ALL` musi mieć test kompletności wariantów (match wymuszający dopisanie do ALL przy nowym wariancie). (d) `[fable]`. Commit: `feat(conformance): convention-conformance contract types (core/conformance)`.

**Step 2 — entries: accounts, admin, apikeys, gateway** `[fable]` (effort: think hard)
(a) W każdym z 4 crate'ów: `pub mod conformance` (plik `src/conformance.rs`) z `pub fn entry() -> core_conformance::Entry`; dodanie `core/conformance` do ich `Cargo.toml` (zwykła zależność — probe'y muszą być dostępne dla harnessu bez feature-flag; wzór `asyncevents::testing`). (b) Przed harnessem, bo harness ich importuje; te 4 są "gorące" (fake'i T7, probe'y T8, parametry T2) i wymagają znajomości wewnętrznych funkcji walidacyjnych. (c) **T8 wymaga sankcjonowanego mini-refactoru w admin i accounts** (finding review #5/#6 — bez tego probe byłby tautologią porównującą const z samym sobą, nie dowodem egzekwowania):
  - admin: capy są dziś inline literals w handlerze logowania (`modules/admin/src/lib.rs:749-751`). Hoist: `pub(crate) const MAX_USERNAME_BYTES: usize = 128; pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;` + czysta `fn login_input_within_caps(username: &str, password: &str) -> bool`, użyta PRZEZ handler ORAZ probe. Zachowanie produkcyjne bez zmian (te same wartości, ta sama semantyka odrzucenia).
  - accounts: consty istnieją (`MAX_EMAIL_BYTES`/`MAX_PASSWORD_BYTES`, `lib.rs:50-51`), ale egzekwowanie jest inline w handlerach register/login (`lib.rs:246`, `lib.rs:320`). Ten sam hoist: czysta fn walidacji współdzielona przez handlery i probe. Probe NIE woła pełnego handlera (przypadek zaakceptowany szedłby do DB na lazy pool).
  - apikeys: zero refactoru — gotowe check-fns (`admin.rs:66`, `store.rs:95`) i publiczny `apikeysapi::MAX_KEY_BYTES`.
  T7 probe: gateway — `RealKeyVerifier::new` z fake'iem `Keys` w stylu `UnavailableVerifier` (przenieść z `modules/gateway/src/tests.rs:99` do zawsze-kompilowanego `conformance.rs`; testy reimportują stamtąd); accounts — probe bez sieci: nieskonfigurowany provider epic ⇒ `Unavailable` (wzór `modules/accounts/src/tests.rs:250-252`). T2: `ArgonParams` z już-publicznych `argon2_params_for_parity_test()` obu crate'ów (istniejący parity-test w `cmd/server` zostaje — nadmiarowość nieszkodliwa; odnotować w commit message). (d) `[fable]`. Commit: `feat(accounts,admin,apikeys,gateway): conformance entries + shared byte-cap validators`.

**Step 3 — entries: pozostałe 8 modułów** `[sonnet]` (effort: default)
(a) `src/conformance.rs` + `pub mod conformance` w: characters, inventory, config, audit, scheduler, match, rating, leaderboard; zależność na `core/conformance` w ich `Cargo.toml`. (b) Po Step 2, żeby kopiować ustalony kształt pliku; przed Step 4 (harness importuje). (c) Wg ROZSTRZYGNIĘTEJ macierzy wyżej (stance'y są decyzją, nie oceną wykonawcy): audit = `EnvValidation(vec![EnvCase{ var: "AUDIT_RETENTION_DAYS", bad_value: "0" }, EnvCase{ …, bad_value: "-3" }])`; match = NA T8 z why jak w macierzy; reszta NA z konkretnymi why. **Pułapka nazewnicza match:** katalog `modules/match`, crate `match_module`, `Module::name()` = `"match"` — `Entry.module` MUSI być `"match"` (klucz diffu to nazwa katalogu/Module::name(), import to `match_module::conformance::entry()`). Sonnet dostaje w prompcie macierz + wzorzec pliku ze Step 2 + zakaz zmian produkcyjnej logiki. (d) `[sonnet]`. Commit: `feat(modules): conformance entries for remaining fortresses`.

**Step 4 — harness `tools/conformance` (package name: `conformancecheck`)** `[fable]` (effort: think hard)
(a) Nowe: `tools/conformance/Cargo.toml` (package `conformancecheck`; deps: `conformance` (core), wszystkie 12 modułów, `checkmodules` (nazwy monolitu — spójnie z rationale "zależymy, nie duplikujemy"), `lifecycle`, `sqlx` (connect_lazy), `bus`) + `src/main.rs` + `src/checks.rs` + `src/tests.rs`. Root workspace members. **Plus edycja `tools/archcheck/src/main.rs:61`:** dopisać `"conformancecheck"` do `GATEWAY_CHECKER_HOSTS` (+ doc const i komunikat rule 3; sprawdzić czy `tools/archcheck` nie pinuje listy w teście) — bez tego rule 3 failuje na dep→`gateway` (blocker z review). (b) Po entries — importuje je wszystkie; rdzeń całości. (c) Cztery fazy:
  1. **Preflight drift (didn't-forget, per-entry log):** `entries()` = ręczna lista `vec![accounts::conformance::entry(), …]`. Diff 3-stronny: katalogi `modules/*` z dysku (wzór `crate_dirs()` archcheck) ↔ `entry.module` nazwy ↔ `Module::name()` z `checkmodules::monolith_modules()` **minus sankcjonowany wyjątek core-infra: `metrics`** (jest w zestawie monolitu, nie jest fortecą — lustrzane z regułą 4 CLAUDE.md "process infrastructure is never declared"; wyjątek jako nazwana const z komentarzem, nie magiczny filtr). Każda rozbieżność osobną linią: `modules/foo on disk has no conformance entry — add foo::conformance::entry() to tools/conformance entries()`. Fail przed jakąkolwiek asercją.
  2. **Macierz kompletności:** każdy entry musi deklarować stance dla każdego `Convention::ALL`; NA bez niepustego `why` = fail.
  3. **Egzekutory.** **Boot T6 wzorcem routecheck, nie `Context::new()`** (blocker z review: DB-backed moduły failują register na DB-less Context zanim dojdzie do walidacji env): `PgPool::connect_lazy(dsn)` + `Context::with_db_and_transport(pool, Arc::new(NoopTransport))` — własna kopia `NoopTransport` w harnessie (w routecheck jest prywatny; NIE hoistować w tym rollout'cie, odnotować duplikację komentarzem z odsyłaczem do `tools/routecheck/src/main.rs:91-94`). **Dyscyplina env jak w routecheck (`main.rs:48-51`):** `fn main` NIE jest `#[tokio::main]`; dla każdego EnvCase: ustaw var → zbuduj świeży single-thread `Runtime` → w nim świeże moduły + `App::build()` → oczekuj Err zawierającego `var` w łańcuchu komunikatu → drop runtime → `remove_var`. (To także zamyka koszt przyszłego edition-2024 `unsafe set_var` w jednym miejscu.) T8 — `!probe(cap) && probe(cap+1)`. T7 — `probe().await == Unavailable` w runtime'ie fazy (Rejected/Other = fail z opisem). T2 — zebrać wszystkie `ArgonParams`, parami równe; pojedynczy Applies = pass z notą.
  4. **Raport:** tabela moduł × konwencja (`applies-pass/applies-FAIL/n-a`), non-zero exit przy jakimkolwiek fail.
  Czyste funkcje (drift-diff, macierz, egzekucja pojedynczego case'a na danych syntetycznych) w `src/checks.rs` z unit testami w `src/tests.rs` — w tym test negatywny: syntetyczny zestaw z brakującym modułem ⇒ drift error z oczekiwanym komunikatem (dowód, że "zapomniałem" faktycznie failuje). Harness NIE potrzebuje żywego Postgresa (lazy pool, walidacja failuje przed I/O) — brak kontencji z zasadą jednego rolloutu, ale nadal uruchamiany sekwencyjnie w verify. (d) `[fable]`. Commit: `feat(conformance,archcheck): conformancecheck harness — drift preflight + convention executors`.

**Step 5 — verify + dokumentacja** `[sonnet]` (effort: default)
(a) `verify.sh`: `simple_stage conformance true cargo run -q -p conformancecheck` (bez cudzysłowów — `simple_stage` bierze słowa, wzór `verify.sh:643` split-proof; blocking stages żyją ~635-643) obok fortress/split-proof; lustrzanie `verify.ps1`. `CLAUDE.md`: linia w Commands (`cargo run -p conformancecheck`), punkt w "Adding a module" (krok: dodaj `conformance.rs` + wpis w `tools/conformance` entries — preflight i tak przypilnuje), wzmianka w Layout `tools/`. Skill `.claude/skills/add-game-module` — dopisać krok (plik skilla zlokalizować Globem). (b) Ostatni krok kodowy — stage musi widzieć działający harness. (c) Mechaniczne wg wzorca istniejących stage'ów. (d) `[sonnet]`. Commit: `chore(verify,docs): blocking conformance stage + module recipe update`.

**Step 6 — weryfikacja końcowa** `[inline]`
(a) Sekwencyjnie (zasada jednego rolloutu — najpierw sprawdzić brak działających cargo/rustc): `cargo build --workspace` → `cargo run -p conformancecheck` (oczekiwane: tabela, exit 0) → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo run -p archcheck` → `cargo test -p conformancecheck -p checkmodules -p admin -p accounts` (ostatnie dwa: hoist walidacji nie zmienił zachowania) → pełny `cargo test --workspace` tylko jeśli wcześniejsze czyste. Dowód negatywny bez dotykania drzewa: unit test driftu ze Step 4 + ręczne `AUDIT_RETENTION_DAYS=0 cargo run -p server` NIE jest potrzebne (T6 wykonuje to samo w harnessie). (b) Zamknięcie. (c) Po multi-subagent rollout: audyt trailerów `git log --format="%h %B" | grep Co-Authored` — `[fable]`→Fable 5, `[sonnet]`→Sonnet 4.6. (d) `[inline]`.

## Ryzyka / decyzje zapisane w planie
- **T6 a env-gates dev:** harness odpala pełny zestaw modułów z czystym env — dev-gates są fail-closed domyślnie, więc build/init przechodzi bez flag (potwierdzone researchem: "FAILS STARTUP unless flag" dotyczy tylko procesu gateway BEZ capability accounts/apikeys — monolityczny zestaw je ma).
- **Nazwy pakietów:** core = `conformance` (katalog `core/conformance`), harness = `conformancecheck` (katalog `tools/conformance`) — brak kolizji, spójne z rodziną `*check`.
- **Zmiana widoczności / hoist walidacji** (admin, accounts): sankcjonowany w Step 2, zachowanie produkcyjne identyczne (te same wartości i semantyka), weryfikowane istniejącymi testami modułów w Step 6.
- **`NoopTransport` zduplikowany** (routecheck ma prywatny) — świadoma duplikacja z komentarzem-odsyłaczem; hoist do współdzielonego miejsca to osobna decyzja poza tym rolloutem.
- Konwencje seam-core z ping-ponga (T3 edge-graces, T5 remote retry, T9 bounded waits) świadomie POZA zakresem — to nie są konwencje per-moduł; mają const-pin/unit testy u autorytetów.

## Review (krok 5 workflow) — rozliczenie
Grumpy-reviewer (fable, think hard) dał 12 findings, werdykt needs-rework; wszystkie BLOCKER/SHOULD-FIX zaadresowane w tej wersji: #1 rule-3 allowlist (Step 4), #2 boot przez lazy-pool+NoopTransport (Step 4), #3 dyscyplina env/runtime per-case jak routecheck (Step 4), #4 wyjątek `metrics` w diffie (Step 4), #5/#6 sankcjonowany hoist walidacji admin/accounts (Step 2), #7 macierz rozstrzygnięta (bez "wykonawca oceni"), #8 simple_stage bez cudzysłowów + poprawne kotwice, #9 nazwy pakietów, #10 dep na checkmodules zamiast server + usunięty zbędny dep `app`, #11 pułapka `match` (Step 3), #12 zamknięte przez #3.

## Weryfikacja end-to-end (definicja "działa")
1. `cargo run -p conformance` — tabela 12 modułów × 4 konwencje, exit 0.
2. Unit test driftu w `tools/conformance` udowadnia czerwony wynik dla syntetycznego "zapomnianego" modułu.
3. `./verify.sh --fast` — nowy stage `conformance` PASS, żaden istniejący stage nie zmienia wyniku.
4. Trailer-audit commitów zgodny z lane'ami.
