# Plan: migracja sync-seam gRPC → edge-RPC po QUIC (msquic via Panama/FFM)

> **Nowy plik w repo (na zatwierdzeniu):** `docs/plans/2026-07-05-1520-quic-msquic-migration-plan.md`.
> **Status:** przeszedł grumpy-review (Opus, ultrathink). Statyczne ABI potwierdzone z `msquic.h`; blocker-y
> B1–B4 (dynamiczne/runtime) + S1–S6 + nity wchłonięte poniżej; log rozwiązań na końcu.

## Context

Wewnętrzny sync-seam `PlayerCharacters.ownerOf` (inventory→characters) jedzie dziś po **gRPC** (PoC, za dużo
maszynerii). Cel: zastąpić go **edge RPC core** (`edge/`, MessagePack, transport-agnostyczny) po **prawdziwym
QUIC** (msquic przez FFM) — realny test stosu, który napisaliśmy, i kasacja gRPC. Feasibility udowodniona
(commit 6ff528c): msquic v2.5.9 (schannel, `G:\tmp\msquic\msquic.dll`) wołalny z JDK 26 przez FFM. `edge` core
(529652a) ma seam `EdgeTransport`/`EdgeConnection`. **Outcome:** proces A serwuje `characters.ownerOf` po QUIC,
proces B woła edge-clientem po QUIC; `POST /inventory/{id}/grant` real=200 / nieistniejąca=400. Monolit bierze
gałąź local (bez QUIC/certu). Weryfikowalne tutaj (2 JVM + Postgres, QUIC=UDP localhost, schannel Windows).

## Research — zweryfikowane fakty (msquic.h/_winuser.h + docs; potwierdzone przez reviewera)

- **Model:** JEDEN trwały **bidi-stream per połączenie**, ramki **4B-BE-len + payload** (msgpack). QUIC = bajtowy
  pipe → reassembly po naszej stronie. **Jeden `StreamSend` per ramka** — cała `[len||payload]` w jednym wywołaniu
  (1 lub 2-elementowa tablica `QUIC_BUFFER`), inaczej współbieżne wysyłki się przeplatają i rozjeżdżają framing.
- **API-table indeksy (potwierdzone):** SetCallbackHandler=2, RegistrationOpen=5, ConfigurationOpen=8,
  LoadCredential=10, ListenerOpen=11/Start=13, ConnectionOpen=15/Start=18/SetConfiguration=19, StreamOpen=21/
  Start=23/Send=25. Status = signed int (**Windows: OK gdy `>=0`**; PENDING=0x703E5 też ≥0).
- **Struktury (x64, potwierdzone):** `QUIC_BUFFER`=16B(ptr@8); `QUIC_SETTINGS`=144B(IsSetFlags@0,
  IdleTimeoutMs@24, **KeepAliveIntervalMs@88**, PeerBidiStreamCount:short@94); `QUIC_CREDENTIAL_CONFIG`=56B(pad4@44);
  `QUIC_CERTIFICATE_HASH`=20B; event `{Type:int@0, pad4, union@8}`. **`QUIC_ADDR` = `SOCKADDR_INET`** (z
  `msquic_winuser.h`, NIE w msquic.h): 28B, `si_family` u16@0, `sin_port` u16 **network-order**@2, IPv4 addr@4;
  `QUIC_ADDRESS_FAMILY_INET=AF_INET=2`, `_UNSPEC=0`.
- **SETTINGS:** serwer `IsSetFlags=(1<<2)|(1<<16)|(1<<18)` = IdleTimeoutMs+KeepAliveIntervalMs+PeerBidiStreamCount;
  `IdleTimeoutMs=30000`, `KeepAliveIntervalMs=15000`, `PeerBidiStreamCount=1`. **Klient też ustawia
  KeepAliveIntervalMs** (nie NULL) — inaczej połączenie idle-outuje między wywołaniami. Rozmiar=144 jako SettingsSize.
- **TLS (schannel):** PEM nie działa. Serwer=`CERTIFICATE_HASH` (20B thumbprint z `New-SelfSignedCertificate`
  `cert:\CurrentUser\My`), Type=1, Flags=0. Klient=Type=0(NONE), Flags=CLIENT|NO_CERT_VALIDATION=0x5.
- **ALPN "edge"** ustawione w `ConfigurationOpen` po OBU stronach, `QUIC_BUFFER.Length=4` (bez NUL) — inaczej
  `ALPN_NEG_FAILURE`.
- **RECEIVE:** `Buffers` = tablica `BufferCount` × `QUIC_BUFFER`; **iterować BufferCount**, każdy
  `Buffer[0..Length)` dopisać do akumulatora, potem ciąć kompletne ramki (wiele ramek/event, częściowa końcówka).
  Bajty ważne TYLKO w callbacku → kopiować natychmiast; zwrócić `SUCCESS` (skopiowaliśmy wszystko).
- **StreamSend lifetime:** struktura `QUIC_BUFFER` + bajty pinowane do `SEND_COMPLETE`, którego
  `ClientContext` = `void*` przekazany w `StreamSend`. Rejestr `long-id→segment`, free w `SEND_COMPLETE`.
- **FFM:** `upcallStub(mh, desc, Arena.global())` dla ~3 singletonowych stubów (bez close-hazardu). **Context =
  long-id wprost w `void*`** (`MemorySegment.ofAddress(id)`, nigdy nie dereferowany) → `ConcurrentHashMap<Long,
  obj>` (bez natywnej komórki). Per-połączenie: `Arena.ofShared()` zamykana **dopiero po SHUTDOWN_COMPLETE**;
  per-send: free w SEND_COMPLETE. Callback: `try/catch(Throwable)→INTERNAL_ERROR`, nie blokować wątku msquic.
  Wątek natywny attach automatyczny. Load dll: `libraryLookup(absPath, Arena.global())`,
  `--enable-native-access=ALL-UNNAMED`.
- **Sekwencja serwer:** Open→RegistrationOpen→ALPN→ConfigurationOpen(settings)→LoadCredential(cert-hash)→
  ListenerOpen(cb)→ListenerStart(alpn, **QUIC_ADDR** port=htons(9100)). Listener CB NEW_CONNECTION→
  SetCallbackHandler(conn,connCb)+ConnectionSetConfiguration→SUCCESS. Conn CB PEER_STREAM_STARTED→
  SetCallbackHandler(stream,streamCb). Stream CB RECEIVE→reassembly→dispatch→StreamSend reply; SEND_COMPLETE→free.
  (PEER_STREAM_STARTED przychodzi dopiero po 1. StreamSend klienta — serwer nie zakłada streamu przed wysyłką.)
- **Sekwencja klient:** Open→RegistrationOpen→ConfigurationOpen(settings+ALPN+keepalive)→LoadCredential(client)→
  ConnectionOpen(cb)→ConnectionStart(cfg, UNSPEC, "host", port /*host-order skalar, NIE QUIC_ADDR*/). Conn CB
  CONNECTED→StreamOpen(NONE=bidi)→StreamStart→StreamSend. **connect() blokuje na latch CONNECTED z timeoutem**
  (SHUTDOWN zamiast CONNECTED = błąd, nie wieczne czekanie).

## Powierzchnia migracji (zweryfikowana)

- **USUŃ:** moduł `characters-grpc/` (proto+build+beans.xml), `characters/PlayerCharactersGrpcService.kt`,
  gRPC-adapter+`@GrpcClient` w `PlayerCharactersProvider.kt`, `quarkus-grpc`+`:characters-grpc` z
  `characters`+`inventory/build.gradle.kts`, allOpen `GrpcService`, `include("characters-grpc")`, klucze
  `quarkus.grpc.*`. Zaktualizuj komentarz w `LocalPlayerCharacters.kt` (odwołuje się do usuwanego GrpcService).
  **ZOSTAW Stork** (`quarkus.stork.characters-service.*` + `admin.characters.url` — admin REST fan-out).
- **DODAJ:** DTO `OwnerOfRequest(characterId:Long)`/`OwnerOfReply(found:Boolean, ownerId:String?)` w
  **`characters-api`** (bez cyklu — `edge` nie zależy od `characters-api`). `characters`+`inventory` dostają
  `implementation(project(":edge"))`.

---

## Sekwencja implementacji

### Krok 0 — Persist plan do repo `[inline]`
Nowy `docs/plans/2026-07-05-1520-quic-msquic-migration-plan.md`.

### Krok 1 — FFM msquic foundation (`edge/.../msquic/`) `[opus]`
- **(a)** `MsQuicLibrary` (dll z resource→temp→`libraryLookup(Arena.global())`, `MsQuicOpenVersion(2)`→api-table,
  `MsQuicClose`), `MsQuicApi` (downcall handle per funkcja, `apiTable.get(ADDRESS, idx*8)`), `Layouts`
  (WSZYSTKIE: QUIC_BUFFER, QUIC_SETTINGS, QUIC_REGISTRATION_CONFIG, QUIC_CREDENTIAL_CONFIG, QUIC_CERTIFICATE_HASH,
  **SOCKADDR_INET/QUIC_ADDR (28B)**, LISTENER/CONNECTION/STREAM_EVENT + gałęzie), `Constants` (statusy, cred/
  stream/send/receive flags, event-type inty, AF_INET=2), `CallbackRegistry` (`ConcurrentHashMap<Long,Any>` +
  `AtomicLong`; **context = long-id wprost w `void*`**, bez natywnej komórki — N2), `Upcalls`
  (`upcallStub` na `Arena.global()` dla listener/conn/stream cb).
- **(b)** Fundament; reszta na tym stoi.
- **(c)** Vendoruj `msquic.dll` do `edge/src/main/resources/native/` (patrz Krok 7 — tu tylko konsumpcja).
  Statusy `>=0`=OK (Windows). Areny: `global()` dla stubów/api-table/rejestracji.
- **(d)** `[opus]` — ABI correctness-critical.
- **Weryfikacja:** `edge/build.gradle.kts` `tasks.test { jvmArgs("--enable-native-access=ALL-UNNAMED") }` (S6);
  test: RegistrationOpen + ConfigurationOpen(**pełne 144B SETTINGS**, sanity `byteSize()==144`) + client-cred
  LoadCredential + close → SUCCESS (N1 — settings round-trip, nie tylko client-NULL).

### Krok 2 — `MsQuicServerTransport : EdgeTransport` `[opus]`
- **(a)** `serve(onConnection)`: ConfigurationOpen(SETTINGS: IdleTimeout+**KeepAlive**+PeerBidiStreamCount=1, ALPN
  "edge" len4)+LoadCredential(**CERTIFICATE_HASH** z thumbprint configu)+ListenerOpen(listenerCb)+ListenerStart
  (ALPN, **QUIC_ADDR**: family=AF_INET(2)@0, sin_port=**htons(port)**@2, addr=127.0.0.1@4 — B1). Listener CB
  NEW_CONNECTION→SetCallbackHandler(conn,connCb)+ConnectionSetConfiguration→SUCCESS. Conn CB PEER_STREAM_STARTED→
  SetCallbackHandler(stream,streamCb)+utwórz `MsQuicConnection`(EdgeConnection)→`onConnection(it)`. Stream CB
  RECEIVE→**pętla po BufferCount, każdy Buffer skopiuj** (B2)→`FrameReassembler` (4B-BE-len; wiele ramek/event +
  częściowa końcówka)→kompletne ramki do `BlockingQueue<ByteArray>`; zwróć SUCCESS (N8).
- **(b)** Serwer — po Kroku 1.
- **(c)** `MsQuicConnection`: `receive()`=take z kolejki (null na SHUTDOWN); `send(frame)`=**jeden StreamSend**
  contiguous `[len||payload]` (B4), rejestr `long-id→segment` free w SEND_COMPLETE (B3). Per-połączenie
  `Arena.ofShared()` zamykana **po SHUTDOWN_COMPLETE** (StreamClose/ConnectionClose) — S4. Kopiuj RECEIVE bajty
  w callbacku.
- **(d)** `[opus]`.

### Krok 3 — `MsQuicClientTransport.connect() : EdgeConnection` `[opus]`
- **(a)** ConfigurationOpen(SETTINGS z **KeepAliveIntervalMs** — nie NULL, S2; ALPN "edge" len4)+LoadCredential
  (**NONE|CLIENT|NO_CERT_VALIDATION=0x5**)+ConnectionOpen(connCb)+ConnectionStart(cfg, UNSPEC, host, port skalar).
  Conn CB CONNECTED→StreamOpen(NONE=bidi)+StreamStart+zwolnij latch. **connect() czeka na latch z timeoutem**;
  SHUTDOWN przed CONNECTED→wyjątek (S3). Stream CB RECEIVE→reassembler→kolejka. Zwróć `MsQuicConnection`.
- **(b)** Klient — po Kroku 1; równolegle z Krokiem 2.
- **(c)** `EdgeClient(connection)` z increment A działa nad tym bez zmian; jeden StreamSend per ramka (B4).
- **(d)** `[opus]`.

### Krok 4 — Self-signed cert + config plumbing `[sonnet]`  *(przesunięte PRZED echo-test — S1)*
- **(a)** `scripts/ensure-cert.ps1`: jeśli brak certu FriendlyName „GameBackend-Edge" w `cert:\CurrentUser\My`
  → `New-SelfSignedCertificate -DnsName localhost -CertStoreLocation cert:\CurrentUser\My ...`; wypisz Thumbprint.
  Config (base): `edge.server.cert-thumbprint=${EDGE_CERT_THUMBPRINT:}`, `edge.server.characters.port=9100`,
  `edge.client.characters.target=${CHARACTERS_EDGE_ADDR:localhost:9100}`. `RoleConfig.isMonolith()` (`"all" in roles`).
  Split-characters bez thumbprint = **fail loudly** przy starcie serwera (N-guard).
- **(b)** Krok 5 i 6 tego potrzebują.
- **(d)** `[sonnet]`.

### Krok 5 — Standalone QUIC echo test (milestone) `[opus]`  *(self-contained)*
- **(a)** Test w `edge`: reuse/utwórz cert (ensure-cert), server transport na localhost:PORT z `EdgeServer`
  handler echo; `MsQuicClientTransport.connect`→`EdgeClient`→round-trip po **realnym QUIC/UDP**; asercja reply+cid.
  Test ustawia `--enable-native-access` (jvmArgs) i thumbprint.
- **(b)** Dowód transportu PRZED migracją; zależy od 2,3,4.
- **(d)** `[opus]` — „działa naprawdę po QUIC".

### Krok 6 — Migracja: usuń gRPC, wepnij edge `[opus]`
- **(a)** USUŃ wg „powierzchni migracji" (+ komentarz w `LocalPlayerCharacters.kt` — N5). DODAJ DTO do
  `characters-api`. `CharactersEdgeServer` (`@Observes StartupEvent`): `if (!isActive("characters") ||
  roleConfig.isMonolith()) return` → QUIC serwer TYLKO w split-characters (monolit=local, bez certu/QUIC);
  rejestruje `characters.ownerOf` (`typedHandler<OwnerOfRequest,OwnerOfReply>`→`local.ownerOf`) w `EdgeServer`
  nad `MsQuicServerTransport`; `ShutdownEvent`→close. `PlayerCharactersProvider` gałąź remote →
  `EdgeRemotePlayerCharacters` z **trwałym `EdgeClient` (lazy connect + reconnect-on-failure)** wołający
  `client.call("characters.ownerOf", OwnerOfRequest(id), OwnerOfReply::class.java)` → `ownerId?.let(UUID::
  fromString)`; reconnect gdy `receive()`=null / proces A restart (S3). `@Blocking` na `InventoryResource.grant`
  zostaje (komentarz: edge-client blokuje).
- **(b)** Po transporcie (2,3) i cert (4).
- **(c)** `edge` leaf bez zmian; monolit local branch (nigdy nie dotyka klienta QUIC). Reuse idiomu `isActive`.
- **(d)** `[opus]`.

### Krok 7 — dll vendoring + launch flags `[sonnet]`
- **(a)** `msquic.dll` (x64) do `edge/src/main/resources/native/msquic.dll` (jedyne miejsce — N6);
  `MsQuicLibrary` ekstrahuje do temp + `libraryLookup`. `install.ps1`: dodaj `--enable-native-access=ALL-UNNAMED`
  do `ArgumentList` `java -jar` (oba tryby); microservices: wywołaj `ensure-cert.ps1`, ustaw `EDGE_CERT_THUMBPRINT`
  na procesie A i `CHARACTERS_EDGE_ADDR='localhost:9100'` na procesie B.
- **(b)** Żeby split ruszył z QUIC.
- **(d)** `[sonnet]`.

### Krok 8 — Weryfikacja end-to-end `[inline]`
Split (`install.ps1 -Mode microservices`): event fanout dalej działa (POST /characters→starter); **ownerOf po
QUIC** (`POST /inventory/{id}/grant` real=200, 999999=refuse 400); grep logów A/B na wątki edge/msquic + brak
gRPC; DB holdings. Monolit: zielony (local, bez QUIC/certu). `./gradlew build` + testy (Krok 1 registration/
settings, Krok 5 echo).

## Log rozwiązań reviewera
- **B1** QUIC_ADDR=SOCKADDR_INET (28B, port network-order) do ListenerStart, nie int — Research + Krok 1/2.
- **B2** RECEIVE iteruje BufferCount, kopiuje każdy buffer — Krok 2.
- **B3** send-buffer lifetime: rejestr long-id→segment, free w SEND_COMPLETE — Research + Krok 2.
- **B4** jeden StreamSend per ramka (contiguous) — Research + Kroki 2/3.
- **S1** cert (Krok 4) PRZED echo-testem (Krok 5) — reorder.
- **S2** KeepAliveIntervalMs po obu stronach (klient nie NULL) — Research + Kroki 2/3.
- **S3** reconnect + connect-timeout — Kroki 3/6.
- **S4** areny: global() dla stubów, per-conn ofShared() close po SHUTDOWN_COMPLETE, per-send free na SEND_COMPLETE.
- **S5** ALPN "edge" len4 po obu stronach — Research + Krok 3.
- **S6** `tasks.test { jvmArgs("--enable-native-access=ALL-UNNAMED") }` — Krok 1.
- **N1–N8** settings round-trip w Kroku 1; context=long-in-void*; sekwencyjny per-conn dispatch (OK dla ownerOf);
  cert same-user caveat; komentarz LocalPlayerCharacters; dedup vendoring (tylko Krok 7); status≥0 Windows-only;
  RECEIVE→SUCCESS.

## Ryzyka / świadome
- **Layouty ABI/unie** — najcięższe; layouty zweryfikowane z nagłówków + reviewer, sanity `byteSize()==sizeof`,
  „read Type→asSlice(8)".
- **Arena lifetime / send lifetime** — use-after-free; ścisła kolejność close po SHUTDOWN_COMPLETE, free na
  SEND_COMPLETE.
- **Cert schannel** — Windows-only (CERTIFICATE_HASH); Linux=OpenSSL+PEM poza zakresem. Same-user (nie service).
- **msquic = parity-swap** vs Netty-quiche pod tym samym `EdgeTransport` — fallback bez zmiany reszty, gdyby FFM
  okazał się zbyt żmudny.
