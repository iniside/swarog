//! Tests for [`fetch_remote_admin`] — the fan-out's tri-state mapping. The Absent
//! case runs end-to-end over a REAL loopback edge (empty dispatch table → the
//! server's unknown-method sentinel → typed `edge::Error::UnknownMethod` →
//! `opsapi::Status::NotFound`), proving the typed path replaces the retired
//! `contains("unknown method")` string sniff without behavior change.

use std::sync::Arc;

use crate::fetch_remote_admin;

/// `unwrap_err` needs `T: Debug` and `adminapi::ItemData` has no `Debug` — unwrap
/// the error arm by hand.
async fn fetch_err(caller: Arc<dyn opsapi::Caller>) -> adminapi::ItemError {
    match fetch_remote_admin(caller).await {
        Err(e) => e,
        Ok(_) => panic!("expected the fetch to fail"),
    }
}

/// A peer that is UP but serves no admin surface: a live internal edge server with
/// nothing registered. The fetch must map it to `Absent` (the admin silently drops
/// the sidebar item), not to an error card.
#[tokio::test]
async fn no_admin_surface_peer_maps_to_absent() {
    let ca = edge::DevCA::generate().unwrap();
    let running = edge::Server::new()
        .listen("127.0.0.1:0".parse().unwrap(), &ca)
        .unwrap();
    let client = edge::Client::dial(running.local_addr(), &ca).await.unwrap();

    let err = fetch_err(Arc::new(client)).await;
    assert!(matches!(err, adminapi::ItemError::Absent), "{err:?}");

    running.close();
}

/// A `Caller` standing in for a DOWN peer: every call fails with the transport's
/// `Unavailable` (what `From<edge::Error>` produces for connect/stream failures).
struct DownCaller;

#[async_trait::async_trait]
impl opsapi::Caller for DownCaller {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
        _retry_mode: opsapi::RetryMode,
    ) -> Result<Vec<u8>, opsapi::Error> {
        Err(opsapi::Error::unavailable("edge: connection: peer down"))
    }
}

/// A genuinely unreachable peer must surface as `Other` (the admin shows an
/// "unavailable" error card), never be swallowed as `Absent`.
#[tokio::test]
async fn peer_down_maps_to_other_error_card() {
    let err = fetch_err(Arc::new(DownCaller)).await;
    assert!(matches!(err, adminapi::ItemError::Other(_)), "{err:?}");
}
