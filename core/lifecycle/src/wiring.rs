use std::collections::HashMap;

/// The runtime-parameterized inputs a `cmd/<name>-svc` (or `cmd/server`) library
/// needs to build its real module list: peer edge addresses for `remote::Stub`
/// factories, and HTTP passthrough origins for the gateway front door. `main.rs`
/// resolves these from env (its own defaults, unchanged from before Step 10) and
/// builds the real `ProcessWiring`; the checker harnesses (`tools/checkmodules`)
/// build a dummy one — `register`/`init` do no I/O, so any placeholder address is
/// safe, the same trick `topiccheck`'s lazy DB pool already relies on.
///
/// Deliberately NOT the 2202 plan's `ProcessSpec{protocol,…}` — there is no
/// protocol fencing to encode, only the plain data a composition root already read
/// from env before this refactor. QUIC/player-edge runtime HANDLES (`Arc<Mutex<…>>`)
/// are never carried here — those stay constructed in `main.rs` and are threaded
/// into a lib's `modules()` as an explicit parameter where needed (`cmd/gateway-svc`,
/// `cmd/server`), so a checker never allocates a real socket.
#[derive(Default, Clone, Debug)]
pub struct ProcessWiring {
    peers: HashMap<String, String>,
    /// A provider's peer edge address SET (round-robin end-state, C2). Distinct from
    /// [`ProcessWiring::peers`] (the single-address map every standalone svc still wires
    /// via [`ProcessWiring::with_peer`]): only the front door's managed boot populates a
    /// SET, so a co-hosted `remote::Stub` can be handed ALL of a provider's live
    /// instances (fed to a `remote::Pool`) instead of one. A single-instance provider is
    /// a one-element set — byte-identical to the old single path.
    peer_sets: HashMap<String, Vec<String>>,
    passthrough: Vec<(String, String)>,
    /// The front door's credential-admission budget (`CREDENTIAL_ADMISSION_TIMEOUT_MS`),
    /// parsed from env by the front processes' `main.rs` (the same place passthrough
    /// origins are resolved). `None` when unset — the gateway module then applies its
    /// own default. Plain data read from env, exactly like the peers/passthrough above;
    /// no runtime handle lives here.
    admission_budget: Option<std::time::Duration>,
}

impl ProcessWiring {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the resolved edge address for `provider` (a `remote::Stub` peer).
    /// `main.rs` calls this once per stub with its env-resolved value.
    pub fn with_peer(mut self, provider: &str, addr: impl Into<String>) -> Self {
        self.peers.insert(provider.to_string(), addr.into());
        self
    }

    /// The resolved edge address for `provider`, or `default` when `main.rs` never
    /// set one (checker mode, via an empty `ProcessWiring`).
    pub fn peer_or(&self, provider: &str, default: &str) -> String {
        self.peers
            .get(provider)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// Records the resolved edge address SET for `provider` — ALL its live instances
    /// (C2 round-robin). The front door's managed boot calls this per edge peer with the
    /// whole `resolve` answer; a `remote::Stub` reads it back via [`peer_set_or`] and
    /// feeds a `remote::Pool`.
    ///
    /// [`peer_set_or`]: ProcessWiring::peer_set_or
    pub fn with_peer_set(mut self, provider: &str, addrs: Vec<String>) -> Self {
        self.peer_sets.insert(provider.to_string(), addrs);
        self
    }

    /// The resolved edge address SET for `provider`, or `default` (mapped to owned
    /// strings) when `main.rs` never set one (checker mode / an empty `ProcessWiring` /
    /// the standalone single-address path, which passes a one-element default). A stored
    /// EMPTY set is returned as-is (a deliberate "known but nothing live" answer the
    /// caller's `Pool` renders un-routable), NOT replaced by the default.
    pub fn peer_set_or(&self, provider: &str, default: &[&str]) -> Vec<String> {
        self.peer_sets
            .get(provider)
            .cloned()
            .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
    }

    /// Records an HTTP passthrough origin (gateway front door: `/admin`,
    /// `/accounts/epic`, …) for `prefix`.
    pub fn with_passthrough(mut self, prefix: &str, origin: impl Into<String>) -> Self {
        self.passthrough.push((prefix.to_string(), origin.into()));
        self
    }

    /// Every recorded passthrough `(prefix, origin)` pair, in registration order.
    pub fn passthrough(&self) -> &[(String, String)] {
        &self.passthrough
    }

    /// Records the front door's credential-admission budget (`main.rs` calls this once
    /// with its env-parsed value). Absent this call the gateway module applies its own
    /// default.
    pub fn with_admission_budget(mut self, budget: std::time::Duration) -> Self {
        self.admission_budget = Some(budget);
        self
    }

    /// The env-parsed credential-admission budget, or `None` when `main.rs` never set
    /// one (checker mode / an unset env var) — the gateway then uses its own default.
    pub fn admission_budget(&self) -> Option<std::time::Duration> {
        self.admission_budget
    }
}
