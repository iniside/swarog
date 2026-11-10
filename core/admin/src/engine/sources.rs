//! Where manifests and calls come from: the process's own providers plus the peer faces
//! a composition root contributed.

use std::sync::Arc;

use controlplane::{ControlPlane, LocalFace, Manifest, Source, PROVIDER_SLOT, SOURCE_SLOT};
use lifecycle::Context;

/// The peer label of this process's own providers.
pub(crate) const LOCAL_PEER: &str = "local";

/// Local first, then every contributed peer face, in contribution order; plus the local
/// face's manifests, read without I/O.
pub(crate) async fn collect(ctx: &Context) -> (Vec<Source>, Option<Vec<Manifest>>) {
    // Local providers go through a LocalFace too, so `Manifest.writes` is enforced by
    // the one authority on the local path exactly as on a peer's.
    let face = LocalFace::new(ctx.contributions(PROVIDER_SLOT));
    let manifests = match face.manifests().await {
        Ok(manifests) => Some(manifests),
        Err(error) => {
            tracing::error!(%error, "admin: local cp.manifests failed; the refresh wave retries it");
            None
        }
    };
    let local = Source { peer: LOCAL_PEER.to_string(), face: Arc::new(face) };
    let mut sources = vec![local];
    sources.extend(ctx.contributions(SOURCE_SLOT));
    (sources, manifests)
}
