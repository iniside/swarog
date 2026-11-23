# Lore v0.14.0: server-side merge for portal merge requests

Source: `EpicGames/lore` tag v0.14.0 (317f48b9), read-only; stock `loreserver`,
Tier 2 auth. Question: can a portal "Merge" button (Swarm/GitHub style) merge
branch A into B with no human working copy? Anchors are Lore repo paths;
"confirmed" = read by the researcher, "hypothesis" = inferred. Grep results are
lower bounds. Companion: `docs/research/2026-11-23-2308-lore-v0-14-integration-summary.md`.

## Answer

Yes, but not by pushing A's head to B with `fast_forward_merge=true`. The general,
correct path is a Swarog-side worker that links `lore-revision`, builds the merge
revision with the same public functions the server uses, and calls `BranchPush`.
On every server-side path any file changed on both sides with different content
is a conflict (no text auto-merge); the author resolves locally by merging B into
A and pushing A.

## Path A — push A's existing head to B with `fast_forward_merge=true`

All in `lore-server/src/grpc/handlers/branch_push.rs` (confirmed):
- `decide_head_move` (:1176-1196): new head only if B's head is the incoming
  revision's first parent (or forced); otherwise, with the flag, merge. No
  ancestry walk.
- `try_fast_forward_merge` uses `original_base = incoming.parent_self()` (:1377),
  `diff3_collect(base, incoming, head)` without text merge (:1388-1395), refuses
  on conflict (:1405-1413), applies onto B's head (:1427-1441). Parents: B head +
  incoming (:1443-1445); metadata copied, branch = B, `merged-by="server"`
  (:1464-1499).
- **Flaw (hypothesis from the code):** for an A with more than one revision past
  the fork, only A's last revision's change is applied while A is recorded as
  merged. Correct only for a single-revision A.
- Content need not be re-uploaded: the incoming revision is read from the repo
  store (:1142-1164); `verify_fragments` only checks addresses exist
  (:1718-1822). No check ties the revision's branch metadata to the target.
- Head update is compare-and-swap with retry (:1305-1345, :1513-1555,
  :1825-1839). `force=true` overwrites concurrent pushes on retry (:1331-1333) —
  never use it.
- Permissions: `write`, or `push-protected` for a protected B (:847-877,
  :1061-1073).
- Locks are not checked on push (grep; `docs/glossary.md:226`).
- Conflict rule (`change.rs:408-446`): both-sides change with different hashes
  conflicts, text or binary. Text merge (`merge.rs:30` `merge3_text`,
  `branch.rs:4086`) is client-only.

RPC-only safe subset: fast-forward when B's head is on A's first-parent history
(push A's revisions in order, `force=false`; not atomic across revisions), and
server merge when A has exactly one revision.

## Path B — worker linking `lore-revision`

- Public (confirmed): `revision::diff3_collect` (revision.rs:2278),
  `branch::diff3_collect` with real common ancestor (branch.rs:4596-4602),
  `commit::merge_tree_changes` (commit.rs:5258),
  `RepositoryContext::new_server_context` (repository.rs:1306); remote stores
  `RemoteImmutableStore`/`RemoteMutableStore` (`store/remote.rs:43,514`).
- Recipe (hypothesis, not built): load A/B heads → `diff3_collect` vs merge
  base → stop on conflicts → `merge_tree_changes` on B's state → parents B+A
  (merge) or B+zero (squash) → upload new blocks + revision (`write`,
  `storage/v1/put.rs:117`) → `BranchPush` with `fast_forward_merge=true` (a
  moved B re-merges correctly because the base is now the old B head).
- Server merges do not weave file history either (:1427-1500), unlike client
  `merge_into` (`merge.rs` ~:5068).
- Dependencies: `lore-revision` is `publish = false` → git dependency on the
  tag. Pulls tonic, quinn, rustls, ring, prost-types, blake3, diffy,
  jsonwebtoken, `lore-transport`, `lore-storage`, `lore-credential`,
  `lore-io`, `lore-proto`. C code: lz4-sys, zstd-sys
  (`lore-storage/Cargo.toml:27-28`), ring, blake3; io-uring and lore-fuse on
  Linux. No aws-lc in `Cargo.lock` (grep). Lore patches quinn-proto and
  glob-match from `vendor/` (`Cargo.toml:307-311`); a consumer must copy that
  `[patch]`.

## Path C — client merge (confirmed, `lore-client/src/cli/commands/branch.rs`, `cli.rs`)

`lore branch switch B` → `lore sync` → `lore branch merge A --message "<msg>"`
(auto-commits without conflicts, :176-182, :1400-1414); on conflicts edit,
`lore branch merge resolve [mine|theirs] <paths>`, `lore commit`; then
`lore push` or `lore push --fast-forward-merge` (:83-90). Author-side: on A,
`lore branch merge B`, resolve, commit, push; then `lore branch merge into B`
(requires B's head already merged into A, `merge.rs:5195-5222`).

## Squash and branch delete

- Squash: Path A no (second parent is always incoming, :1445); Path B yes
  (second parent zero, push without `fast_forward_merge`, retry on decline);
  Path C no flag (:170-210).
- Delete source: `RevisionService.BranchDelete` (`revision.proto:16`), `write`
  (`revision/v1/branch_delete.rs:59`), refused if delete-protected
  (:169-171). CLI: `lore branch archive A`.

## Lore docs

No LEP proposes server-side merge, merge queue or change-request hosting. The
roadmap promises a review web client (`docs/roadmap.md:81-85`). The low-level
revision API LEP puts merge out of scope (`2026-05-14-low-level-revision-api.md:68`);
the revision-graph LEP names `fast_forward_merge` as the mechanism (:446).
Revision metadata already has `reviewed-by`, `merged-by` and a change-request
id key (`lore-revision/src/metadata.rs:122-134`).

## Cannot do on any server-side path

Resolve conflicts; auto-merge both-sides text edits; honour file locks
(the merge must check locks itself if wanted); atomic multi-revision
fast-forward on Path A.

## Open

- Passing a Tier 2 per-repo token into `RemoteConnections` from an embedding
  process (CLI uses `LORE_ACCESS_TOKEN`, cli.rs:91-94).
- Whether remote stores plug into `new_server_context` and whether
  `RepositoryWriteToken::server` is usable outside Lore.
- One integration test confirming the Path A multi-revision flaw.
- Whether `lore branch merge` works in a bare clone (a worker running the stock CLI).

## Not checked

Client multi-revision push loop (`lore-revision/src/branch/push.rs:1818-2075`);
`HookPoint::BranchPush`; ThinClient `RevisionDiff` handler; `branch_diff.rs:106-146`;
links and layers; history queries over unwoven merge revisions; remote
`revision_tree` load; `lore-proto` crate weight alone.
