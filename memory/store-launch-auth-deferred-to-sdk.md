---
name: store-launch-auth-deferred-to-sdk
description: "Store/launcher login (Epic Store, Steam) is deferred to a future engine SDK; backend stays a pure token verifier"
metadata: 
  node_type: memory
  type: project
  originSessionId: 177a4d5a-9a36-469c-9744-6d6a166e60b1
---

The store/launcher login flow (game launched from Epic Games Store, Steam, etc.)
is deferred to a future engine SDK. The backend does NOT change for it — it stays
a pure token verifier.

**Why:** Store/launcher differences are entirely client-side (how the client
obtains a token). Backend only ever verifies a token and maps it to a player_id.
The native-client entry point already exists: `POST /accounts/login/epic` (client
gets a token from the EOS SDK and posts the raw token; no browser OAuth). The web
UI + EAS OAuth-redirect flow was only so the linking could be demoed without an SDK.

**How to apply:** When the SDK lands, the store path yields an EOS **Connect** ID
token (`sub` = PUID, JWKS at `…/auth/v1/oauth/jwks`), whereas the current web path
uses **Epic Account Services** (`sub` = Epic Account ID, JWKS at
`…/epic/oauth/v1/.well-known/jwks.json`). These are different subjects. The OIDC
verifier is config-driven, so add a second configured verifier (e.g. provider
`epic-connect`) alongside the existing `epic` — core unchanged. Open decision for
then: which is the leading identity (PUID vs Epic Account ID), and whether to link
both to one player. See [[reference-local-postgres]] for the running setup.
