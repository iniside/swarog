# Status: audyt 2026-07-13 — remediacja UKOŃCZONA

Plan: [2026-07-13-1415-audit-remediation-plan.md](../plans/2026-07-13-1415-audit-remediation-plan.md)
(+ errata: odzyskana recenzja Codexa, precyzja R1). Zakres: 16 defektów audytu
(14 potwierdzonych + 2 sporne w tanim wariancie) + 12 findings z odzyskanej
recenzji planu Codexa. **46 commitów** od `af87f73` do `040e281`.
Finalny `verifyctl --all --strict`: wszystkie stage'e PASS (fuzz SKIP — platforma).

## Proces

Każdy krok: implementacja (sonnet/opus wg tagu) → adversarial review (opus +
Codex, niezależnie) → follow-up commit zamykający punch listę → dopiero następny
krok. Checkpointy `--all --strict` po fazach A i C oraz na końcu. Wyjątek
procesowy wart odnotowania: checkpoint fazy C złapał regresję Send-provability
niedostrzegalną per-crate; pierwsza naprawa (d2b202a) przepisała produkcyjny
lifecycle pod ograniczenie test-harnessu — obie recenzje nazwały to inwersją
authority i została częściowo cofnięta (32c7c01: core/app i lifecycle
byte-identical, fix u źródła w testach przez tokio::select!).

## Kroki → commity

| Krok | Commity (impl + follow-up) |
|------|---------------------------|
| 1 archcheck rule 9 | 23ec41a + 5e8ae52 (exactly-one-target tripwire) |
| 2 topiccheck (topic,version) | 89353e6 + 75d4042 (tokenizer boundary, multi-define) |
| 3 WorkspaceLayout | 0446df4 + c2546dc (csharp sibling, compile-time root) |
| 4 splitproof liveness | 49757dc + 029c5a5 (pre-spawn port probe, Windows fixture) |
| 5 golden fingerprint | d48d982 + 627c33e (doc-hidden + public-api bless) |
| 5b golden hardening (Codex C7/C9/C10) | be3aae3 + 606d321 (None-sample tripwire) |
| 6 inventory Quantity | 5bfe36b + 2c01e3c (cap-conflict 409) |
| 7 scheduler ceiling | ae749d6 + fb23e4e (jedna stała, VALID restore) |
| 8 HTTP conn ownership | 5943eed + 2904a90 (axum-server Handle) |
| 9 asyncmig lock_timeout | 23a8aee + 4b7d41c (detach po failed ROLLBACK, pool=1 proof) |
| — checkpoint C repair | d2b202a → 32c7c01 (częściowy revert, patrz wyżej) |
| 10 admission budget | cec0d73 + 6fe744a (=0 fail-loud) |
| 11 BOOT_TIMEOUT | 6d505e2 + ee11078 |
| 12 pool budget | d96e4bf + 5a0ab31 (env-injection z budżetu, itemizowany reserve) |
| 13 env_rate_pair + invalidation | e5c1b7f + 65c28b4 (overflow-bail, boot-level C20) |
| 14 pattern_overlaps | bfd7e9d (bez follow-upu — obie recenzje czyste) |
| 15a Epic 503 | 5bef6ab + 2d10a2d (sub-sekundowy dead-pool) |
| 15b normalize_username | 7b93247 + e7695aa (echo znormalizowanej nazwy) |
| 16 edge typed code | 8d6c5e9 + 755f9f8 (errata w planie 2026-07-11-1602 Step 7) |
| 17 docs + docs-current tripwire | 14345e3 + 2ede767 + cba0a71 (dead-gate sekcji zamknięty) |
| 18 retention NOT EXISTS | 5694b40 + 040e281 (ORDER BY floor-upward, failure-safe teardown) |

DB rollout: `DROP SCHEMA inventory CASCADE` + `DROP SCHEMA scheduler CASCADE`
(nowe CHECK-i; wipe-over-migrations).

## Odrzucone findings recenzentów (z zapisanym uzasadnieniem w commit bodies)

- Lock catalog↔GC dla retention (kontrakt MinRetention = tylko days; residual
  within-statement window zaakceptowany i udokumentowany).
- Per-service composition tabeli dedicated (nad-aproksymacja myli się tylko w
  stronę latencji; per-svc tabela = nowa powierzchnia didn't-forget).
- Scenariusze splitproof dla inventory-degrade / hung-verifier (wymagałyby
  test-backdoorów w produkcyjnych svc).
- Druga sesja zamiast triggera w teście retention (trigger commit'uje z batchem 1
  = dokładnie naprawione okno inter-batch).
- None-samples per Option jako derive-machinery (zamiast: text-scan tripwire).

## Known gaps (jawne, przeniesione z planu + nowe z recenzji)

1. Domain-op unbounded awaity (inventory owner_of, match mmr×2, admin fan-out) —
   objęte HTTP_REQUEST_TIMEOUT_MS; spójne z celowym unbounded player-dispatch.
2. ALLOW_UNSUBSCRIBED/ALLOW_INPROCESS_DEFINED kluczują po topicu (puste dziś).
3. Edge QUIC drain = close-and-hope dla nie-transportowych hangów (kandydat na
   osobny rollout — sibling kroku 8).
4. `Module::start` bez ogólnego bounda w core/lifecycle (tylko stop jest
   bounded) — nazwany przez recenzję kroku 11.
5. policy_columns i32 clamp (osiągalne tylko z literałów), splitproof text-sniff
   oracle (po kroku 16 można przepiąć na typed code — follow-up), WorkspaceLayout
   nie parsuje .cargo/config.toml, golden: scalar-collapse/first-element/enum
   (udokumentowane w GOLDEN_HEADER).

## Infra: Codex w pętli review

Codex działał w ~70% wywołań (sandbox padał sporadycznie na CreateProcessAsUserW
1312); recenzja planu ukończyła się, ale wynik trzeba było odzyskać z rolloutu
sesji na dysku (~/.codex/sessions). Przy padach kontynuowano z samym opusem per
protokół planu.
