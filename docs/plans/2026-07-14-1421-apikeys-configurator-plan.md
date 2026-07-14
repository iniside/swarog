# Plan: apikeys — pełny configurator zarządzania kluczami (role + hashing + wzbogacony generyczny remote-editable admin + ops catalog)

> Ten plik jest kanonicznym planem tej sesji; po zatwierdzeniu skopiuję go do
> `docs/plans/2026-07-14-HHMM-apikeys-configurator-plan.md` (CLAUDE.md: plany żyją w repo).

## Context

`apikeys` dziś udostępnia JEDNĄ capability — `apikeysapi::Keys::lookup_key` (wire-only,
`#[retry_safe]`, konsumowaną przez gateway, `modules/gateway/src/keys.rs:315-336`). Cały CRUD
kluczy to **lokalne domknięcie** `apply_edit` w `modules/apikeys/src/admin.rs` na płaskim
`adminapi::Form`; sekrety są plaintext, wpisywane ręcznie, wyświetlane zawsze. Chcemy **pełny
configurator w admin portalu**: wiele typów klienta/serwera, wielokrotnego użytku **role**
(nazwane polityki), generowane sekrety pokazywane raz, edytowalny **w obu topologiach**.

### Kluczowa decyzja architektoniczna (zatwierdzona): wzbogacić GENERYCZNY adminapi

Recenzja ultrathink wskazała, że bespoke strona sprzęga domenowo-agnostyczny portal
(`modules/admin` renderuje WSZYSTKIE moduły z `adminapi::SLOT`, nie zależy od żadnego
`<name>api`) z jednym modułem. Zamiast tego **wzbogacamy sam kontrakt `adminapi`** tak, by
formularze były (a) typowane (checkbox/select), (b) **edytowalne zdalnie** i (c) mogły zwrócić
**one-time reveal** (show-once sekret). Efekt kaskadowy — **znika cała potrzeba osobnej
capability `KeyManager`**: skoro nowy `admin.adminSubmit` uruchamia submit-closure prowidera
**server-side (w `apikeys-svc`, gdzie store jest lokalny)**, apikeys robi CRUD swoim własnym
domknięciem, bez wystawiania go jako cross-process RPC. admin zostaje generyczny (bez importu
`apikeysapi`, bez chirurgii nav), a remote-admin-write staje się **reużywalnym seamem** dla
każdego kolejnego modułu.

### Dlaczego tak (mapa nakładających się systemów)

- **Dlaczego nie osobna capability `KeyManager`?** Przy generycznym `admin.adminSubmit` mutacje
  jadą tym samym edge-face co istniejący read-only `admin.adminData` (`api/admin/api/src/lib.rs:44-49`),
  a domknięcie prowidera odpala się lokalnie w jego procesie. Druga capability + stub + coupling
  admin→apikeysapi byłyby zbędne. Jedyna cross-process capability apikeys pozostaje `Keys`.
- **Dlaczego nie płaski `Form` przez edge?** `Form.submit` jest `#[serde(skip)]`
  (`api/admin/api/src/lib.rs:187-188`) i remote render wymusza `form: None`
  (`modules/admin/src/lib.rs:1236-1242`) — **remote-admin-write to greenfield** (zweryfikowane).
  Płaskie pola name/value nie wyrażą checkboxów/dropdownu. Wzbogacamy kontrakt zamiast forka.
- **Dlaczego nie argon2 dla sekretów?** Klucze API są wysokoentropijne → **deterministyczny
  SHA-256** (indeksowalny O(1) lookup, show-once, prefix). `argon2` (password-KDF) nie da się
  indeksować. Conformance `ArgonParity` dla apikeys zostaje **N/A** z nowym uzasadnieniem.
- **Dlaczego nie live-katalog operacji z krawędzi gateway?** Byłby to pierwszy w repo
  inbound-edge na gateway-svc (mTLS server identity, port, fleet) — zły koszt/wartość dla UI-nicety.
  Zamiast tego **build-time generowany, freshness-gated artefakt** z tych samych `route_bindings()`
  metadanych rpc, których używa gateway (`topiccheck` już je czyta, `golden.rs:118`). To samo
  jedno źródło prawdy, zero runtime-edge, zero dryfu.

### Roles model (zatwierdzony: referencyjny / znormalizowany)

Klucz **wskazuje** rolę (`keys.role → roles.name` FK w schemacie `apikeys`). Edycja polityki roli
**natychmiast** zmienia efektywną politykę wszystkich jej kluczy (`lookup_key` JOIN-uje
`keys→roles`; propagacja przez istniejący 5s cache gatewaya — ten sam bound co dziś, brak
hazardu kolejności — potwierdzone przez recenzję).

---

## Dispatch lanes (do zatwierdzenia z planem)

- **core-implementer** (`subagent_type:"core-implementer"`, `model:"opus"`) — wzbogacenie
  kontraktu `adminapi` (typed `Field`, `SubmitOutcome`, opt-in `AdminSubmit` `#[rpc]`),
  generyczny remote-submit w module `admin`, generator ops-catalog. Cross-seam / authority-first.
- **[opus]** — apikeys store/service (crypto, CAS-by-revision, role CRUD, FK mapping), wzbogacony
  apikeys admin Form + `AdminSubmit` impl, splitproof assertion. Domenowe, correctness-critical.
- **[sonnet]** — mechaniczne: migracja `SubmitFn` Ok-type w 7 modułach, re-bless baseline'ów
  (public-api adminapi/apikeysapi, contract-golden), wpisy `conformance/policy.rs`, docstringi
  CLAUDE.md/AGENTS.md + `api/apikeys/api` doc, `Cargo.toml` deps.

Adversarial review (core-reviewer, `model`≥implementer) po każdym zaakceptowanym diffcie; przy
diffach dotykających verify-stage (Faza C generator, splitproof) dołóż proof-auditor. Trailer
audyt po rolloucie.

---

## Faza A — Wzbogacenie generycznego `adminapi` + generyczny remote-write

### Step 1 — Kontrakt `adminapi`: typed fields + SubmitOutcome + opt-in AdminSubmit  `[core-implementer]`
**(a) co:** `api/admin/api/src/lib.rs`, `api/admin/rpc/src/lib.rs`, baseline
`docs/reference/public-api-baseline/adminapi.txt`, `docs/reference/contract-golden/contracts.txt`.
**(b) dlaczego pierwsze:** wszystko dalej (admin render, apikeys form) stoi na tym kontrakcie.
**(c) jak:**
- `Field` (`lib.rs:193-198`) — dołóż `kind: FieldKind` (default `Text`, additive) + `options:
  Vec<FieldOption>`. `enum FieldKind { Text, Select, CheckboxGroup }`,
  `struct FieldOption { value, label, checked }`. Istniejące moduły domyślnie `Text` — brak
  regresji renderu.
- `SubmitOutcome { reveal: Vec<RevealItem> }`, `RevealItem { label, value }` — one-time wartości
  do pokazania po submit (show-once). `SubmitFn` (`lib.rs:78`) zmienia Ok-type z `()` na
  `SubmitOutcome`.
- Nowy **opt-in** `#[rpc(prefix="admin")]` trait `AdminSubmit { async fn admin_submit(&self,
  id: String, params: Params) -> Result<SubmitOutcome, Error> }` — **BEZ `#[retry_safe]`**
  (mutacja, `RetryMode::Never`, fail-closed). Prowider implementuje opcjonalnie; brak
  rejestracji metody → `edge::Error::UnknownMethod` → `NotFound` → admin degraduje do read-only
  (istniejące zachowanie, graceful). Konflikt CAS → `Error::conflict(...)`
  (**`opsapi::Status::Conflict` JUŻ istnieje**, `opsapi:154-157`, `http()=>409`, wykluczony z
  `is_definitive_answer` — żadnego dotyku `core/*`, żadnego re-bless opsapi).
- Remote content path: `admin_data` prowidera zwraca teraz `Content` z `form: Some(Form{fields
  (typed), hidden, submit: None})` — struktura do renderu + allowlist, bez domknięcia (submit
  jedzie przez `admin.adminSubmit`).
- Re-bless: `--bless-public-api` (adminapi), `--bless-contract-golden` (nowa metoda `admin.adminSubmit`).

### Step 2 — Migracja `SubmitFn` Ok-type w istniejących modułach  `[sonnet]`
**(a) co:** 7 modułów z lokalnym submit-closure (`config`, `accounts`, `characters`, `inventory`,
`scheduler`, `audit`, oraz `apikeys` — ten i tak przepisywany w Fazie B). Każde `Ok(())` w
domknięciu submit → `Ok(adminapi::SubmitOutcome::default())`.
**(b) dlaczego teraz:** Step 1 zmienia sygnaturę; drzewo nie kompiluje bez tego sweepa.
**(c) jak:** czysto mechaniczne, additive; `SubmitOutcome::default()` = pusty reveal (brak zmiany
zachowania dla modułów bez show-once).

### Step 3 — Moduł `admin`: render typed fields + generyczny remote submit  `[core-implementer]`
> **ERRATA (recenzja core-reviewer po Step 1, commit 1ec7e1c):** remote-submit MUSI być
> **per-provider closure na `adminapi::Item`** (jak `remote_fetch`), NIE pojedyncza capability
> `dyn AdminSubmit` pod kluczem `admin.admin_submit` — ten klucz paniekuje przy 2. prowiderze i
> w splicie miswroutuje (config→apikeys-svc). Więc Step 3 dodatkowo: (i) dodaj
> `remote_submit: Option<RemoteSubmitFn>` na `adminapi::Item` (additive, kolejny public-api
> re-bless), (ii) przerób `admin_remote_factory(provider)` by wypełniał OBA closure (fetch+submit,
> oba dialują TEN prowider po `id`), (iii) **usuń** wysłany w Step 1 `admin_submit_remote_factory`
> (zły autorytet, obecnie martwy). `register_admin_submit` (server-side) zostaje bez zmian. To
> upraszcza Step 4 (admin_stub już woła `admin_remote_factory` per-provider).
**(a) co:** `api/admin/api/src/lib.rs` (`Item` + `RemoteSubmitFn`), `api/admin/rpc/src/lib.rs`
(`admin_remote_factory` rework, usuń `admin_submit_remote_factory`), `modules/admin/src/lib.rs`
(`item_post` `:1021-1108`, `page_view` `:1225-1276`, `resolve_items` `:1184-1221`), szablony
minijinja (render `Select`/`CheckboxGroup`), `modules/admin/src/tests.rs`. **admin ZOSTAJE
domenowo-agnostyczny — brak importu `apikeysapi`, brak `requires()`, brak chirurgii nav.**
**(b) dlaczego teraz:** to jest reużywalny seam; apikeys (Faza B) tylko go wypełnia danymi.
**(c) jak:**
- Render: szablony obsługują `FieldKind::{Select,CheckboxGroup}` (dziś tylko text input).
- `page_view`: **przestań** hardcodować remote `form: None` (`:1236-1242`) — renderuj remote form
  (typed fields) z akcją POST na zwykłe `/admin/:slug`.
- `item_post`: `gate`+`check_csrf` **przed** decyzją local/remote **zostaje** (kontrakt kolejności,
  splitproof AD4, `:1028-1032`). Dla remote itemu (dziś 405, `:1041`): zbuduj `Params`
  (allowlist z fetchowanego remote-form field/hidden names), wołaj `admin.adminSubmit(slug,
  params)` przez edge prowidera; `Ok(SubmitOutcome)` → 303 + wyrenderuj `reveal` (show-once);
  `UnknownMethod`/`NotFound` → 405 (prowider bez remote-write, read-only); `Err(Conflict)` → 409.
  Lokalny submit-closure path zwraca teraz `SubmitOutcome` (pokaż reveal tak samo).
- **Prove-the-failing-branch:** test że remote item BEZ zarejestrowanego `admin_submit` renderuje
  read-only i POST → 405; że POST bez `_csrf` odrzucony przed jakąkolwiek próbą edge-call.

### Step 4 — cmd/admin-svc: (po erracie Step 3 — minimalny / prawdopodobnie no-op)  `[core-implementer]`
**(a) co:** `cmd/admin-svc/src/lib.rs` (`admin_stub` `:14-20`).
**(b) dlaczego:** po erracie remote-submit jedzie per-provider closure na Itemie z
`admin_remote_factory` — który `admin_stub` JUŻ woła per prowidera. Więc Step 4 to najwyżej
potwierdzenie, że przerobiony `admin_remote_factory` (Step 3) niesie oba closure; brak nowego
factory, brak zmian `fleet.rs`. Prowider bez `admin_submit` impl → edge `UnknownMethod`→`NotFound`
w runtime (graceful read-only). admin `require` nie dochodzi — fan-out po `adminapi::SLOT`.

---

## Faza B — apikeys: data model + rich admin form (backed by KeyManager-less local store)

### Step 5 — Schema (roles+keys, hash/prefix/role/revision) + store  `[opus]`
**(a) co:** `modules/apikeys/src/lib.rs` (`SCHEMA_DDL` `:37-53`), `modules/apikeys/src/store.rs`,
`modules/apikeys/Cargo.toml` (deps `sha2`, `rand`; `hex`/base64 wg workspace).
**(b) dlaczego teraz:** rich form (Step 6) i lookup stoją na nowym store.
**(c) jak:** fresh-boot DDL (wipe-strategy — istniejące dev DB: `DROP SCHEMA apikeys CASCADE`):
```sql
CREATE TABLE IF NOT EXISTS apikeys.roles (
  name text PRIMARY KEY, policy text NOT NULL,
  revision bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS apikeys.keys (
  name text PRIMARY KEY,
  secret_hash text NOT NULL UNIQUE,                       -- sha256 hex (64), indeksowany
  prefix text NOT NULL,                                   -- pierwsze ~12 znaków do wyświetlania
  role text NOT NULL REFERENCES apikeys.roles(name),      -- ZNORMALIZOWANE; FK to autorytet
  revision bigint NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
  revoked_at timestamptz);
```
Store (zastępuje business-column-CAS `store.rs:114-161` przez CAS-by-`revision`):
- `lookup(presented) -> Option<KeyRecord>`: `hash=sha256_hex(presented)`, `SELECT k.name,
  r.policy FROM apikeys.keys k JOIN apikeys.roles r ON r.name=k.role WHERE k.secret_hash=$1 AND
  k.revoked_at IS NULL`. Wire `KeyRecord{name,policy}` **bez zmian** (kontrakt `Keys` i baseline
  nietknięte — `policy` to teraz rozwiązana polityka roli). Gateway guard długości *presented*
  klucza (`keys.rs:270`) **zostaje** — ortogonalny do hashowania kolumny.
- `generate_secret() -> (secret, hash, prefix)`: 32 bajty `OsRng` → enkoduj, prefiks `ak_`;
  `hash=sha256_hex`; `prefix=&secret[..12]`.
- CRUD ról/kluczy CAS-by-revision: `UPDATE ... SET ..., revision=revision+1, updated_at=now()
  WHERE name=$1 AND revision=$expected` → `rows_affected()==1` albo konflikt.
- **Mapowanie błędów (uwaga recenzenta #7 — FK jest autorytetem):** `23505`(unique)→Conflict na
  create_key/create_role; **`23503`(FK)→Conflict/Invalid** na create_key (rola nie istnieje),
  set_key_role (rola docelowa nie istnieje), delete_role (rola użyta). `EXISTS`-guard w
  delete_role to tylko ładniejszy komunikat — realną ochroną przed wyścigiem
  create_key↔delete_role jest FK NO-ACTION (`23503`).
- `list_keys`/`list_roles`: **bez sekretów** (tylko prefix). **Usuń** ścieżkę zwracającą surowy
  `key` (`store.rs:43-62`, `admin.rs Cell::mono(&r.key)`).
- **Dev seed** (`lib.rs:60-76,213-221`): utwórz role `dev-client`(=`DEV_CLIENT_POLICY`) i
  `dev-server`(=`full`) **przed** kluczami (kolejność FK); klucze `dev-client`/`dev-server` z
  `secret_hash=sha256("dev-key-client"/"dev-key-server")`, `role=...`. `X-Api-Key: dev-key-server`
  dalej działa (hashuje się do zapisanego hasza) → K3/K4/smoke bez zmian. Upsert self-healing
  zachowany.
- Zachowaj **luźną** walidację polityki (dziś `admin.rs:41-52` — dopuszcza metody jeszcze
  nieserwowane; katalog z Fazy C jest podpowiedzią UI, nie twardym gate).

### Step 6 — apikeys admin Item: rich typed form + AdminSubmit impl + show-once  `[opus]`
**(a) co:** `modules/apikeys/src/admin.rs` (przepisz), `modules/apikeys/src/lib.rs` (`init`
`:184-210`), `modules/apikeys/src/{tests,admin_tests,store_tests}.rs`, `modules/apikeys/src/conformance.rs`.
**(b) dlaczego teraz:** wypełnia wzbogacony seam z Fazy A danymi apikeys.
**(c) jak:**
- Jeden `build_content(svc, with_submit)` budujący typowane pola: **Select** roli (opcje z
  `list_roles`), **CheckboxGroup** metod przy edycji polityki roli (opcje z ops-catalog artefaktu,
  Faza C), pola create-key/create-role/revoke. Lokalnie `with_submit=true` (domknięcie backed by
  store); remote (`admin_data`) `with_submit=false` (struktura + wartości, submit przez edge).
- Submit-closure → store CRUD; po `create_key` zwróć `SubmitOutcome{reveal:[RevealItem{"secret",
  wygenerowany}]}` (**show-once** — sekret NIGDY z `list`, tylko w tej odpowiedzi).
- `impl adminapi::AdminSubmit for Service` (opt-in) — deleguje: re-render `build_content(svc,true)`
  → `form.submit` → `submit(params).await`. Zarejestruj metodę w istniejącym `EDGE_SLOT`
  `EdgeReg` obok `admin.adminData` (`lib.rs:200-208`). **apikeys DALEJ kontrybuuje swój
  `adminapi::Item`** (nav/index/AD3b bez zmian — item teraz rich + zdalnie edytowalny). Kontrakt
  `Keys` i jego edge-face bez zmian.
- **conformance.rs (uwaga #6):** sekrety są teraz server-generated — znika ścieżka
  caller-supplied-secret-creation, więc `conformance_key_rejected` (creation-side cap) **repoint
  albo retire**; zachowaj gateway lookup-side cap (`keys.rs:270`, presented key). Zaktualizuj
  probe pod nowy store.
- **Prove-the-failing-branch (split-aware w Step 9):** stale `expected_revision` → Conflict, NIE
  pisze; lookup po revoke → `None`; edycja polityki roli zmienia efektywną politykę klucza (JOIN);
  delete-role-in-use → Conflict (FK); sekret pojawia się TYLKO w odpowiedzi create, nigdy w list.
- **Recovery (uwaga #13):** create nie-idempotentny + zgubiona odpowiedź `admin.adminSubmit` =
  klucz istnieje, sekret utracony; retry trafia PK (`23505`→Conflict) i nie pozna sekretu →
  operator revoke+recreate pod nową nazwą. Udokumentuj w admin form (hint).
- **Autoryzacja (uwaga #14):** `admin.adminSubmit` (wire-only, process-trust, mTLS) niesie teraz
  mutacje — ten sam model zaufania co `admin.adminData`, tylko zapis; auth operatora
  (sesja/CSRF) egzekwowany w admin-svc PRZED edge-call.

---

## Faza C — Ops-catalog (generowany artefakt, freshness-gated) — odłączalna

> Bez niej configurator działa: edytor polityki roli przyjmuje wolny tekst metod (luźna
> walidacja). Z nią CheckboxGroup dostaje żywą listę. Zero runtime-edge (odrzucona krawędź
> gateway-svc).

### Step 7 — Generator + data crate + freshness stage + konsumpcja w apikeys  `[core-implementer]` + `[sonnet]`
**(a) co:** nowy `tools/opscatalog-gen` (walka `route_bindings()` wszystkich `<name>rpc`,
filtr `#[http]`), generowany neutralny data-crate `opscatalog` (`pub const OPERATIONS:
&[OpInfo]`, transport-free, domenowo-neutralny — importowalny przez moduły jak `opsapi`), nowy
verifyctl freshness stage (diff jak contract-golden), `modules/apikeys/Cargo.toml` (+dep
`opscatalog`), `modules/apikeys/src/admin.rs` (CheckboxGroup opcje z `opscatalog::OPERATIONS`).
**(b) dlaczego po Fazie B:** apikeys form konsumuje katalog; generator/artefakt muszą istnieć.
**(c) jak:** generator emituje deterministyczny `OPERATIONS` (method/verb/path/auth z
`opsapi::Operation`, `lib.rs:265`); freshness stage FAIL gdy artefakt != regeneracja (re-bless
komendą). apikeys renderuje checkboxy z tej listy; luźna walidacja pozostaje (operator może
zaznaczyć/dopisać metodę jeszcze nieserwowaną). `topiccheck` już czyta `route_bindings` —
generator korzysta z tego samego dostępu.

---

## Faza D — tooling / weryfikacja

### Step 8 — conformance + baseline'y + docs  `[sonnet]`
- `tools/conformance/src/policy.rs` `apikeys()` (`:178-204`): `ArgonParity` **zostaje N/A** z
  uzasadnieniem „apikeys hashuje wysokoentropijne sekrety SHA-256, nie password-KDF". Repoint/retire
  creation-side `InputByteCaps` CapCase (`:188-192`) i podstawę input-policy (`:58`, połowa
  „apikeys creation enforces MAX_KEY_BYTES" jest już nieprawdą — zostaje tylko gateway lookup-side);
  dodaj CapCase dla pól role/policy jeśli zewnętrznie osiągalne przez admin POST.
- Re-bless: `adminapi.txt` (typed Field, SubmitOutcome, AdminSubmit), `apikeysapi.txt` (bez zmian
  kontraktu `Keys` — potwierdź brak diffu), `contract-golden/contracts.txt` (nowe `admin.adminSubmit`).
  `--bless-public-api` + `--bless-contract-golden`.
- Docs (uwaga #12 — stale doc): `CLAUDE.md:279,281` + `AGENTS.md:278,280` +
  `modules/apikeys/src/lib.rs:34-36` + `api/apikeys/api/src/lib.rs:17-26` (`MAX_KEY_BYTES` doc
  odwołuje się do `insert_tx`/DDL CHECK na `key`, które znikają — zaktualizuj na SHA-256 hash +
  prefix + gateway presented-key guard, role-referenced policy). `docs-current` stage tego pilnuje.

### Step 9 — splitproof: cross-process rich-form write w splicie  `[opus]` (+ proof-auditor)
**(a) co:** `tools/splitproof/src/main.rs` (nowa named assertion + aktualizacja AD3b `:1315-1318`,
którego treść strony się zmienia na rich form).
**(c) jak:** wzoruj na cross-process starter-grant/wipe (`:930-996`, `poll_count` `:1767`) + form-CSRF
z `[M3b]` `:330-370` (`extract_form_fields`): sesja `admin` → `POST` create-role potem create-key
**przez gateway-svc→admin-svc→(edge `admin.adminSubmit`)→apikeys-svc** → wyciągnij show-once sekret
z odpowiedzi → `sqlx::query_scalar` na `apikeys.keys`/`apikeys.roles` przez `pool` (`:545`):
asercja że wiersz powstał, `role` wskazuje utworzoną rolę, i **`secret_hash == sha256(reveal
sekret)`** oraz że ŻADNA kolumna nie trzyma sekretu w cleartext (uwaga #11). To dowód
**at-risk-topology** (split) generycznego remote-write seamu. **Współbieżny tokio + długi
timeout** (nie shell-loop) by nie wskrzesić deadlocku AD2b/AD2c (`:1228-1306`).

---

## Sekwencja i „green" (uwaga recenzenta #8)

- **Faza A jest samodzielnie zielona** (wzbogacenie adminapi + migracja Ok-type + generyczny
  remote-write, bez apikeys) — istniejące moduły dalej działają, remote-write jest opt-in i
  nieużywany dopóki jakiś prowider nie zarejestruje `admin_submit`. Można ją scommitować i
  odpalić pełny verifyctl.
- **Faza B jest zielona po A** — apikeys wypełnia seam; AD3b/K3/K4 trzymają (item wciąż obecny,
  dev-keye działają). apikeys `admin.rs` przepisany atomowo (stary flat form → rich form) — brak
  stanu pośredniego łamiącego split-proof (inaczej niż pierwotny plan usuwający Item).
- Faza C i D po B.

## Verification (end-to-end) — wg `safe-verification`, jeden rollout na raz

1. **Per-crate, po każdej fazie:** `cargo test -p adminapi -p admin`; `cargo test -p apikeysapi
   -p apikeys`; `cargo test -p opscatalog`. Testy w `src/tests.rs`; plane-testów nie mieszać.
2. **Static seams:** `archcheck` (admin DALEJ nie importuje żadnego `<name>api` — kluczowa
   inwariantna, potwierdź; apikeys→apikeysapi/apikeysrpc jak dziś), `requirecheck` (admin BEZ
   nowego `requires`), `codegen-freshness` (glue `admin.adminSubmit` + artefakt ops-catalog),
   `contract-golden`, `public-api` (re-blessed adminapi), `conformance`.
3. **Fresh DB:** `DROP SCHEMA apikeys CASCADE` (+ reseed) — nowy DDL fresh-boot-only.
4. **Terminalnie JEDEN manifest:** `cargo run -p verifyctl -- --all --strict` (split-proof z nową
   cross-process asercją + public-api + topiccheck). At-risk path — monolith-only demo to NIE dowód.
5. **Ręczny smoke (monolith, `devctl up monolith`):** zaloguj do `/admin`; utwórz rolę z polityką
   zawierającą `leaderboard.topScores` (uwaga #15), utwórz klucz tej roli, potwierdź show-once;
   `curl localhost:8080/leaderboard -H "X-Api-Key: <nowy-sekret>"` → 200; po revoke → 401.

## Self-check przed „done"
adminapi/admin/apikeys build+test zielone w monolicie i splicie; admin **bez** importu
`<name>api` (archcheck); każdy istniejący submit-closure zmigrowanny na `SubmitOutcome`;
apikeys w `cmd/server`, `cmd/apikeys-svc`, checkmodules Split (auto), processctl fleet, nowej
named splitproof assertion, conformance policy; ops-catalog artefakt freshness-gated. Trailer
audyt: `git log -N --format="%h %B" | grep Co-Authored` == lane każdego commita.
