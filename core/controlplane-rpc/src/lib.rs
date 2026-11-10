//! `controlplanerpc` — the generated edge glue for [`controlplane::ControlPlane`] plus
//! the two helpers core uses to serve and reach a process's control-plane face.

use std::sync::Arc;

use controlplane::*;

controlplane::cp_control_plane_meta!(rpc_macro::generate_glue);

/// Installs the `cp.*` edge handlers for `providers` on `server`.
pub fn register_face(server: &mut edge::Server, providers: Vec<Arc<dyn Provider>>) {
    control_plane_rpc::register_server(server, Arc::new(LocalFace::new(providers)));
}

/// The `remote::RemoteFactory` that contributes `peer`'s face to [`SOURCE_SLOT`].
pub fn remote_factory(peer: &'static str) -> remote::RemoteFactory {
    Box::new(move |ctx, caller| {
        ctx.contribute(
            SOURCE_SLOT,
            Source {
                peer: peer.to_string(),
                face: Arc::new(control_plane_rpc::Client::new(caller)),
            },
        );
    })
}
