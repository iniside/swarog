// Fake data shared by every mockup screen. Deployment: Emberline Studio, game "Ashfall".
window.DATA = (() => {
  const users = [
    { id: "7f3a9c21", handle: "aiko.tanaka", seatSince: "2025-03-11", name: "Aiko Tanaka", email: "aiko@emberline.gg", kind: "staff", status: "active", providers: ["google", "epic"], created: "2025-03-11", lastSeen: "4 min ago", country: "JP", loreRole: "Lead", color: "#d9488a", characters: 3, wallet: { EMB: 12400, GLD: 850, TOK: 12 } },
    { id: "1b84e0d7", handle: "marta.k", seatSince: "2024-11-02", name: "Marta Kowalczyk", email: "marta@emberline.gg", kind: "staff", status: "active", providers: ["google"], created: "2024-11-02", lastSeen: "now", country: "PL", loreRole: "Admin", color: "#4f5bd5", characters: 1, wallet: { EMB: 0, GLD: 0, TOK: 0 } },
    { id: "c02d5f19", handle: "jonas.l", seatSince: "2024-11-03", name: "Jonas Lindqvist", email: "jonas@emberline.gg", kind: "staff", status: "active", providers: ["google"], created: "2024-11-03", lastSeen: "18 min ago", country: "SE", loreRole: "Lead", color: "#0f9488", characters: 2, wallet: { EMB: 300, GLD: 40, TOK: 0 } },
    { id: "9e61aa04", handle: "diego.r", seatSince: "2025-01-20", name: "Diego Ramos", email: "diego@emberline.gg", kind: "staff", status: "active", providers: ["google", "epic"], created: "2025-01-20", lastSeen: "1 h ago", country: "ES", loreRole: "Developer", color: "#c2630f", characters: 4, wallet: { EMB: 5200, GLD: 120, TOK: 3 } },
    { id: "44f0b3e8", handle: "priya.n", seatSince: "2025-02-14", name: "Priya Nair", email: "priya@emberline.gg", kind: "staff", status: "active", providers: ["google"], created: "2025-02-14", lastSeen: "2 h ago", country: "IN", loreRole: "Developer", color: "#7c3aed", characters: 0, wallet: { EMB: 0, GLD: 0, TOK: 0 } },
    { id: "5a7c19d2", handle: "tomasz.w", seatSince: "2026-06-03", name: "Tomasz Wrona", email: "tomasz@emberline.gg", kind: "staff", status: "active", providers: ["google"], created: "2025-05-06", lastSeen: "yesterday", country: "PL", loreRole: "Developer", color: "#2563eb", characters: 1, wallet: { EMB: 90, GLD: 0, TOK: 0 } },
    { id: "e8d24c61", handle: "elena.p", seatSince: "2026-09-01", name: "Elena Petrova", email: "elena@contractor.io", kind: "staff", status: "active", providers: ["google"], created: "2026-08-19", lastSeen: "3 d ago", country: "BG", loreRole: "Viewer", color: "#be123c", characters: 0, wallet: { EMB: 0, GLD: 0, TOK: 0 } },
    { id: "b39f7e10", handle: "sam.o", seatSince: "2026-08-11", name: "Sam Okafor", email: "sam@emberline.gg", kind: "staff", status: "suspended", providers: ["google"], created: "2025-07-30", lastSeen: "21 d ago", country: "NG", loreRole: "Developer", color: "#65a30d", characters: 1, wallet: { EMB: 0, GLD: 0, TOK: 0 } },
    { id: "3c90e5ab", handle: "NightOwl_77", name: "NightOwl_77", email: "—", kind: "player", status: "active", providers: ["epic"], created: "2026-02-08", lastSeen: "12 min ago", country: "DE", loreRole: null, color: "#0891b2", characters: 5, wallet: { EMB: 48210, GLD: 3120, TOK: 44 } },
    { id: "d71b2f86", handle: "kestrel", name: "kestrel", email: "—", kind: "player", status: "active", providers: ["google"], created: "2026-04-17", lastSeen: "1 h ago", country: "US", loreRole: null, color: "#9333ea", characters: 2, wallet: { EMB: 6100, GLD: 410, TOK: 2 } },
    { id: "0a5e8c33", handle: "ash_and_ember", name: "ash_and_ember", email: "—", kind: "player", status: "banned", providers: ["epic"], created: "2026-01-29", lastSeen: "9 d ago", country: "BR", loreRole: null, color: "#dc2626", characters: 3, wallet: { EMB: 0, GLD: 0, TOK: 0 } },
    { id: "f6c4d097", handle: "MiraVoss", name: "MiraVoss", email: "—", kind: "player", status: "active", providers: ["epic", "google"], created: "2025-12-02", lastSeen: "now", country: "PL", loreRole: null, color: "#059669", characters: 6, wallet: { EMB: 91300, GLD: 7720, TOK: 130 } },
    { id: "28be71f5", handle: "guest-28be71f5", name: "guest-28be71f5", email: "—", kind: "player", status: "active", providers: ["guest"], created: "2026-10-22", lastSeen: "6 min ago", country: "FR", loreRole: null, color: "#64748b", characters: 1, wallet: { EMB: 500, GLD: 0, TOK: 0 } },
    { id: "a419d3c0", handle: "Volkhv", name: "Volkhv", email: "—", kind: "player", status: "active", providers: ["epic"], created: "2025-10-14", lastSeen: "3 h ago", country: "UA", loreRole: null, color: "#ca8a04", characters: 2, wallet: { EMB: 22040, GLD: 1310, TOK: 9 } },
  ];
  const byHandle = Object.fromEntries(users.map(u => [u.handle, u]));

  const repos = [
    { name: "ashfall-game", desc: "Main UE5 project (Content + Source)", branch: "main", lastPush: "6 min ago", pushBy: "diego.r", locks: 14, branches: 23, protected: 3 },
    { name: "ashfall-art", desc: "Source art: .psd, .blend, .spp", branch: "main", lastPush: "41 min ago", pushBy: "aiko.tanaka", locks: 9, branches: 6, protected: 1 },
    { name: "ashfall-audio", desc: "Wwise project + stems", branch: "main", lastPush: "3 h ago", pushBy: "tomasz.w", locks: 2, branches: 4, protected: 1 },
    { name: "engine-fork", desc: "Emberline UE5 engine fork", branch: "ember/5.6", lastPush: "yesterday", pushBy: "jonas.l", locks: 0, branches: 11, protected: 2 },
    { name: "tools-pipeline", desc: "Build scripts, DCC exporters", branch: "main", lastPush: "2 d ago", pushBy: "priya.n", locks: 1, branches: 8, protected: 1 },
    { name: "marketing-capture", desc: "Trailer capture levels", branch: "main", lastPush: "12 d ago", pushBy: "elena.p", locks: 0, branches: 2, protected: 0 },
  ];

  const locks = [
    { id: "L-2291", repo: "ashfall-game", file: "Content/Maps/Frostreach/Frostreach_P.umap", owner: "diego.r", branch: "main", since: "2026-10-22 08:14", age: "6 h", desc: "Blocking out north gate encounter" },
    { id: "L-2290", repo: "ashfall-game", file: "Content/Maps/Frostreach/Frostreach_Lighting.umap", owner: "aiko.tanaka", branch: "main", since: "2026-10-22 07:52", age: "7 h", desc: "Lighting pass v3" },
    { id: "L-2284", repo: "ashfall-art", file: "Characters/Warden/SK_Warden_Body.blend", owner: "aiko.tanaka", branch: "main", since: "2026-10-21 16:30", age: "22 h", desc: "Rig fix for cloth sim" },
    { id: "L-2279", repo: "ashfall-game", file: "Content/Blueprints/AI/BT_Warden.uasset", owner: "priya.n", branch: "feature/warden-ai", since: "2026-10-21 11:02", age: "1 d", desc: "" },
    { id: "L-2270", repo: "ashfall-game", file: "Content/UI/WBP_Inventory.uasset", owner: "tomasz.w", branch: "main", since: "2026-10-20 13:45", age: "2 d", desc: "Inventory grid rework" },
    { id: "L-2241", repo: "ashfall-art", file: "Environment/Frostreach/T_Ice_Cliff_D.psd", owner: "sam.o", branch: "main", since: "2026-10-01 09:20", age: "21 d", desc: "Texture repaint", stale: true },
    { id: "L-2238", repo: "ashfall-game", file: "Content/Maps/Hub/Hub_Ashgate.umap", owner: "sam.o", branch: "main", since: "2026-09-30 17:05", age: "22 d", desc: "", stale: true },
    { id: "L-2296", repo: "ashfall-audio", file: "Wwise/Actor-Mixer Hierarchy/Creatures.wwu", owner: "tomasz.w", branch: "main", since: "2026-10-22 10:40", age: "4 h", desc: "Warden VO hookup" },
    { id: "L-2297", repo: "tools-pipeline", file: "exporters/blender/ember_fbx.py", owner: "jonas.l", branch: "main", since: "2026-10-22 12:02", age: "2 h", desc: "" },
    { id: "L-2286", repo: "ashfall-art", file: "Props/Forge/SM_Anvil.blend", owner: "diego.r", branch: "main", since: "2026-10-21 18:11", age: "20 h", desc: "LOD pass" },
  ];

  const branches = [
    { name: "main", head: "c41f9e2", by: "diego.r", when: "6 min ago", ahead: 0, behind: 0, prot: true, locks: 13 },
    { name: "release/0.9", head: "8a02d7b", by: "jonas.l", when: "2 d ago", ahead: 0, behind: 214, prot: true, locks: 0 },
    { name: "release/0.8", head: "f19ce03", by: "jonas.l", when: "34 d ago", ahead: 0, behind: 1180, prot: true, locks: 0 },
    { name: "feature/warden-ai", head: "2be6d41", by: "priya.n", when: "1 h ago", ahead: 37, behind: 12, prot: false, locks: 1 },
    { name: "feature/frostreach-lighting", head: "77d0a19", by: "aiko.tanaka", when: "41 min ago", ahead: 18, behind: 3, prot: false, locks: 0 },
    { name: "feature/inventory-grid", head: "e0c4482", by: "tomasz.w", when: "3 h ago", ahead: 9, behind: 40, prot: false, locks: 0 },
    { name: "spike/nanite-foliage", head: "5d19ab0", by: "diego.r", when: "19 d ago", ahead: 4, behind: 802, prot: false, locks: 0 },
  ];

  const roles = [
    { name: "Viewer", desc: "Read and download", builtin: true, actions: ["read", "presign"], users: 1 },
    { name: "Developer", desc: "Day-to-day contributors", builtin: true, actions: ["read", "write", "presign"], users: 4 },
    { name: "Lead", desc: "push-protected (honoured after LEP)", builtin: true, actions: ["read", "write", "presign", "push-protected", "migrate"], users: 2 },
    { name: "Admin", desc: "Full control incl. destructive ops", builtin: true, actions: ["read", "write", "presign", "push-protected", "migrate", "admin", "owner", "obliterate"], users: 1 },
  ];
  const actions = [
    { key: "read", desc: "Clone, fetch, browse history", access: true },
    { key: "write", desc: "Push to unprotected branches, take locks", access: true },
    { key: "presign", desc: "Issue presigned blob URLs (CI, CDN)" },
    { key: "push-protected", desc: "Push to protected branches (after LEP; today only service accounts bypass PROTECT)" },
    { key: "migrate", desc: "Run repository format migrations" },
    { key: "admin", desc: "Force unlock, settings; who may call BranchProtect is not yet traced" },
    { key: "owner", desc: "Release any lock regardless of owner" },
    { key: "obliterate", desc: "Permanently purge files from history" },
  ];
  // repo overrides: handle -> repo -> role | "none"
  const overrides = {
    "elena.p": { "marketing-capture": "Developer", "ashfall-art": "none", "engine-fork": "none" },
    "priya.n": { "tools-pipeline": "Lead" },
    "tomasz.w": { "ashfall-audio": "Lead" },
    "diego.r": { "engine-fork": "Viewer" },
    "sam.o": { "ashfall-game": "Viewer" },
  };

  const loreEvents = [
    { t: "14:08", day: "Today", type: "push", who: "diego.r", repo: "ashfall-game", branch: "main", text: "pushed 3 commits", detail: "c41f9e2 Frostreach: north gate blockout · 412 MB", tone: "accent", icon: "up" },
    { t: "13:27", day: "Today", type: "push", who: "aiko.tanaka", repo: "ashfall-art", branch: "main", text: "pushed 1 commit", detail: "a0e7712 Warden body rig fix · 1.8 GB", tone: "accent", icon: "up" },
    { t: "12:40", day: "Today", type: "unlock", who: "marta.k", repo: "ashfall-game", branch: "main", text: "force-unlocked Content/Core/DA_GameRules.uasset", detail: "owner was sam.o (suspended) · reason: owner offboarded", tone: "err", icon: "unlock" },
    { t: "12:02", day: "Today", type: "lock", who: "jonas.l", repo: "tools-pipeline", branch: "main", text: "locked exporters/blender/ember_fbx.py", detail: "", tone: "warn", icon: "lock" },
    { t: "11:15", day: "Today", type: "protect", who: "marta.k", repo: "ashfall-game", branch: "release/0.9", text: "protected branch release/0.9", detail: "pushes now require push-protected", tone: "warn", icon: "shield" },
    { t: "10:40", day: "Today", type: "lock", who: "tomasz.w", repo: "ashfall-audio", branch: "main", text: "locked Wwise/Actor-Mixer Hierarchy/Creatures.wwu", detail: "Warden VO hookup", tone: "warn", icon: "lock" },
    { t: "18:22", day: "Yesterday", type: "push", who: "jonas.l", repo: "engine-fork", branch: "ember/5.6", text: "pushed 12 commits", detail: "Merge upstream 5.6.1 hotfix · 96 MB", tone: "accent", icon: "up" },
    { t: "16:30", day: "Yesterday", type: "lock", who: "aiko.tanaka", repo: "ashfall-art", branch: "main", text: "locked Characters/Warden/SK_Warden_Body.blend", detail: "Rig fix for cloth sim", tone: "warn", icon: "lock" },
    { t: "15:02", day: "Yesterday", type: "branch", who: "diego.r", repo: "ashfall-game", branch: "spike/old-terrain", text: "deleted branch spike/old-terrain", detail: "head 91ac3e0 · 0 open locks", tone: "err", icon: "branch" },
    { t: "10:40", day: "Yesterday", type: "branch", who: "priya.n", repo: "ashfall-game", branch: "feature/warden-ai", text: "created branch feature/warden-ai", detail: "from main @ 3d1f0ab", tone: "info", icon: "branch" },
  ];

  const services = [
    { name: "admin-svc", module: "admin", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "29 ms", restarts: 0 },
    { name: "gateway-svc", module: "gateway", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "38 ms", restarts: 0 },
    { name: "accounts-svc", module: "accounts", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "21 ms", restarts: 0 },
    { name: "characters-svc", module: "characters", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "17 ms", restarts: 0 },
    { name: "inventory-svc", module: "inventory", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "24 ms", restarts: 0 },
    { name: "wallet-svc", module: "wallet", state: "down", version: "0.42.1", uptime: "—", p99: "—", restarts: 7, note: "connection refused · last ok 3 min ago" },
    { name: "friends-svc", module: "friends", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "12 ms", restarts: 0 },
    { name: "groups-svc", module: "groups", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "15 ms", restarts: 0 },
    { name: "party-svc", module: "party", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "9 ms", restarts: 0 },
    { name: "notifications-svc", module: "notifications", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "14 ms", restarts: 0 },
    { name: "mail-svc", module: "mail", state: "degraded", version: "0.42.1", uptime: "6 d 4 h", p99: "880 ms", restarts: 0, note: "SMTP relay throttling · 312 queued" },
    { name: "storage-svc", module: "storage", state: "running", version: "0.42.1", uptime: "2 d 1 h", p99: "61 ms", restarts: 1 },
    { name: "match-svc", module: "match", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "19 ms", restarts: 0 },
    { name: "rating-svc", module: "rating", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "8 ms", restarts: 0 },
    { name: "leaderboard-svc", module: "leaderboard", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "11 ms", restarts: 0 },
    { name: "config-svc", module: "config", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "4 ms", restarts: 0 },
    { name: "scheduler-svc", module: "scheduler", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "6 ms", restarts: 0 },
    { name: "audit-svc", module: "audit", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "10 ms", restarts: 0 },
    { name: "apikeys-svc", module: "apikeys", state: "running", version: "0.42.1", uptime: "6 d 4 h", p99: "5 ms", restarts: 0 },
    { name: "lore-svc", module: "lore", state: "running", version: "0.1.0", uptime: "1 d 2 h", p99: "33 ms", restarts: 0 },
  ];

  const apiRoles = [
    { name: "game-server", scopes: ["match.report", "rating.write", "inventory.grant", "wallet.grant", "storage.write"], keys: 3 },
    { name: "ci-build", scopes: ["lore.presign", "lore.read", "config.read"], keys: 2 },
    { name: "analytics-export", scopes: ["audit.read", "wallet.ledger.read", "accounts.read"], keys: 1 },
    { name: "support-bot", scopes: ["accounts.read", "mail.send", "notifications.send"], keys: 1 },
  ];
  const apiKeys = [
    { id: "ak_9f2c", name: "dedicated-eu-west", role: "game-server", prefix: "swg_live_9f2c…", created: "2026-06-02", by: "marta.k", lastUsed: "now", status: "active" },
    { id: "ak_31d0", name: "dedicated-us-east", role: "game-server", prefix: "swg_live_31d0…", created: "2026-06-02", by: "marta.k", lastUsed: "now", status: "active" },
    { id: "ak_77e1", name: "dedicated-staging", role: "game-server", prefix: "swg_live_77e1…", created: "2026-08-14", by: "jonas.l", lastUsed: "2 h ago", status: "active" },
    { id: "ak_c4a8", name: "horde-agents", role: "ci-build", prefix: "swg_live_c4a8…", created: "2026-10-22", by: "marta.k", lastUsed: "4 min ago", status: "active" },
    { id: "ak_05bb", name: "nightly-cook", role: "ci-build", prefix: "swg_live_05bb…", created: "2026-03-19", by: "priya.n", lastUsed: "9 h ago", status: "active" },
    { id: "ak_e2f7", name: "metabase", role: "analytics-export", prefix: "swg_live_e2f7…", created: "2026-01-11", by: "marta.k", lastUsed: "1 d ago", status: "active" },
    { id: "ak_1a6d", name: "zendesk-bridge", role: "support-bot", prefix: "swg_live_1a6d…", created: "2025-12-04", by: "sam.o", lastUsed: "62 d ago", status: "revoked" },
  ];

  const mail = [
    { id: "m-88213", to: "MiraVoss", template: "season_reward", subject: "Season 4 rewards", state: "pending", attempts: 0, next: "in 12 s", err: "" },
    { id: "m-88207", to: "kestrel", template: "season_reward", subject: "Season 4 rewards", state: "retrying", attempts: 3, next: "in 2 min", err: "421 4.7.0 Try again later (relay throttled)" },
    { id: "m-88199", to: "NightOwl_77", template: "purchase_receipt", subject: "Your receipt #A-55102", state: "retrying", attempts: 2, next: "in 40 s", err: "421 4.7.0 Try again later (relay throttled)" },
    { id: "m-88180", to: "Volkhv", template: "season_reward", subject: "Season 4 rewards", state: "failed", attempts: 5, next: "—", err: "550 5.1.1 Mailbox unavailable" },
    { id: "m-88172", to: "ash_and_ember", template: "ban_notice", subject: "Account action", state: "parked", attempts: 5, next: "—", err: "recipient banned · parked by policy" },
    { id: "m-88166", to: "guest-28be71f5", template: "welcome", subject: "Welcome to Ashfall", state: "sent", attempts: 1, next: "—", err: "" },
    { id: "m-88160", to: "aiko.tanaka", template: "playtest_invite", subject: "Playtest Fri 24 Oct", state: "sent", attempts: 1, next: "—", err: "" },
  ];

  const subs = [
    { id: "audit.lore-branch-pushed.v1", topic: "lore.branch.pushed", module: "audit", checkpoint: 18_442, head: 18_442, lag: "0", state: "running" },
    { id: "notifications.lore-resource-locked.v1", topic: "lore.resource.locked", module: "notifications", checkpoint: 9_204, head: 9_210, lag: "6", state: "running" },
    { id: "wallet.on_match.v1", topic: "match.finished", module: "wallet", checkpoint: 40_118, head: 40_391, lag: "273", state: "lagging" },
    { id: "notifications.mail-sent.v1", topic: "mail.sent", module: "notifications", checkpoint: 88_160, head: 88_166, lag: "6", state: "running" },
    { id: "inventory.on_purchase.v1", topic: "wallet.debited", module: "inventory", checkpoint: 12_904, head: 13_022, lag: "118", state: "paused", poison: { seq: 12_905, err: "decode: unknown currency code \"TOK2\" at $.amount.currency", at: "2026-10-22 09:41" } },
    { id: "leaderboard.on_rating.v1", topic: "rating.updated", module: "leaderboard", checkpoint: 220_418, head: 220_418, lag: "0", state: "running" },
    { id: "storage.player-deleted.v1", topic: "player.deleted", module: "storage", checkpoint: 311, head: 311, lag: "0", state: "running" },
  ];

  const audit = [
    { t: "14:12:08", actor: "marta.k", topic: "lore.permission", action: "override.set", target: "user:e8d24c61 → repository:marketing-capture", detail: "Viewer → Developer" },
    { t: "13:58:41", actor: "apikey:ak_c4a8", topic: "lore.presign", action: "presign.issue", target: "repository:ashfall-game", detail: "214 URLs · ttl 15m" },
    { t: "12:40:02", actor: "marta.k", topic: "lore.lock", action: "lock.force_unlock", target: "repository:ashfall-game · DA_GameRules.uasset", detail: "owner sam.o · reason: owner offboarded" },
    { t: "11:15:20", actor: "marta.k", topic: "lore.branch", action: "branch.protect", target: "repository:ashfall-game · release/0.9", detail: "PROTECT set" },
    { t: "10:02:13", actor: "jonas.l", topic: "wallet", action: "grant", target: "user:f6c4d097", detail: "+500 EMB · reason: CS ticket #4471" },
    { t: "09:44:57", actor: "system", topic: "events", action: "subscription.paused", target: "inventory.on_purchase.v1", detail: "poison event seq 12905" },
    { t: "09:30:00", actor: "jonas.l", topic: "config", action: "flag.update", target: "flag:season4.enabled", detail: "false → true · rev 41" },
    { t: "08:51:36", actor: "marta.k", topic: "apikeys", action: "key.create", target: "apikey:ak_c4a8", detail: "horde-agents · role ci-build" },
    { t: "08:12:09", actor: "aiko.tanaka", topic: "accounts", action: "admin.login", target: "admin portal", detail: "google · 83.4.x.x" },
    { t: "07:00:00", actor: "scheduler", topic: "scheduler", action: "fire", target: "daily.leaderboard_reset", detail: "ok · 1.2 s" },
  ];

  const ledger = [
    { t: "2026-10-22 14:02", who: "MiraVoss", cur: "EMB", amt: -1200, bal: 91300, reason: "store.purchase · Frost Warden skin", ref: "tx_8a1f" },
    { t: "2026-10-22 13:40", who: "NightOwl_77", cur: "GLD", amt: 340, bal: 3120, reason: "match.reward · ranked win", ref: "tx_8a1c" },
    { t: "2026-10-22 10:02", who: "MiraVoss", cur: "EMB", amt: 500, bal: 92500, reason: "admin.grant · CS ticket #4471 (jonas.l)", ref: "tx_8a03" },
    { t: "2026-10-22 09:41", who: "kestrel", cur: "TOK", amt: -1, bal: 2, reason: "store.purchase · reroll token", ref: "tx_89f7" },
    { t: "2026-10-21 22:19", who: "Volkhv", cur: "GLD", amt: 120, bal: 1310, reason: "quest.complete · Ashgate Bounty", ref: "tx_89c2" },
    { t: "2026-10-21 20:05", who: "ash_and_ember", cur: "EMB", amt: -18400, bal: 0, reason: "admin.revoke · chargeback (marta.k)", ref: "tx_89a0" },
    { t: "2026-10-21 18:30", who: "aiko.tanaka", cur: "EMB", amt: 10000, bal: 12400, reason: "admin.grant · playtest budget (marta.k)", ref: "tx_8991" },
  ];

  const currencies = [
    { code: "EMB", name: "Embers", kind: "premium", decimals: 0, supply: "41.8 M", holders: "182 k", cap: "—" },
    { code: "GLD", name: "Gold", kind: "soft", decimals: 0, supply: "903 M", holders: "211 k", cap: "—" },
    { code: "TOK", name: "Reroll tokens", kind: "event", decimals: 0, supply: "2.1 M", holders: "64 k", cap: "999 / player" },
  ];

  const schedules = [
    { id: "daily.leaderboard_reset", cron: "0 7 * * *", target: "leaderboard.reset", next: "tomorrow 07:00", last: "ok", lastAt: "07:00", enabled: true },
    { id: "season4.rewards", cron: "once 2026-10-22 12:00", target: "mail.enqueue_batch", next: "—", last: "ok", lastAt: "12:00", enabled: true },
    { id: "lore.billing_snapshot", cron: "0 * * * *", target: "lore.snapshot_usage", next: "15:00", last: "ok", lastAt: "14:00", enabled: true },
    { id: "storage.gc", cron: "30 3 * * *", target: "storage.collect_orphans", next: "tomorrow 03:30", last: "fail", lastAt: "03:30", enabled: true },
    { id: "accounts.purge_deleted", cron: "0 4 * * *", target: "accounts.purge", next: "tomorrow 04:00", last: "ok", lastAt: "04:00", enabled: true },
    { id: "weekend.xp_boost", cron: "0 18 * * FRI", target: "config.set_flag", next: "Fri 18:00", last: "ok", lastAt: "Fri 18:00", enabled: false },
  ];
  const fires = [
    { at: "14:00:00", id: "lore.billing_snapshot", result: "ok", dur: "0.4 s", note: "6 repos · 13.29 TB · 8 seats" },
    { at: "13:00:00", id: "lore.billing_snapshot", result: "ok", dur: "0.4 s", note: "6 repos · 13.27 TB · 8 seats" },
    { at: "12:00:00", id: "season4.rewards", result: "ok", dur: "8.1 s", note: "182 044 mails enqueued" },
    { at: "07:00:00", id: "daily.leaderboard_reset", result: "ok", dur: "1.2 s", note: "4 boards" },
    { at: "04:00:00", id: "accounts.purge_deleted", result: "ok", dur: "2.7 s", note: "31 accounts purged" },
    { at: "03:30:00", id: "storage.gc", result: "fail", dur: "30.0 s", note: "timeout: storage-svc deadline exceeded" },
  ];

  const config = [
    { key: "season4.enabled", type: "bool", value: "true", rev: 41, by: "jonas.l", at: "09:30" },
    { key: "match.max_party_size", type: "int", value: "4", rev: 12, by: "marta.k", at: "Oct 03" },
    { key: "store.featured_rotation", type: "json", value: "[\"frost_warden\",\"ash_blade\"]", rev: 88, by: "diego.r", at: "Oct 21" },
    { key: "storage.save_quota_mb", type: "int", value: "64", rev: 3, by: "marta.k", at: "Sep 11" },
    { key: "xp.weekend_multiplier", type: "float", value: "1.5", rev: 9, by: "scheduler", at: "Oct 17" },
    { key: "motd.text", type: "string", value: "Season 4: Frostreach is live!", rev: 22, by: "aiko.tanaka", at: "12:01" },
    { key: "lore.max_lock_age_days", type: "int", value: "14", rev: 2, by: "marta.k", at: "Oct 20" },
  ];

  const billing = {
    months: ["May", "Jun", "Jul", "Aug", "Sep", "Oct"],
    seats: [5, 6, 6, 7, 8, 8],
    tb: [6.2, 7.4, 8.9, 10.6, 12.1, 13.29],
    storeTB: 13.29,
    volumeTB: 20,
  };

  return { users, byHandle, repos, locks, branches, roles, actions, overrides, loreEvents, services, apiRoles, apiKeys, mail, subs, audit, ledger, currencies, schedules, fires, config, billing };
})();
