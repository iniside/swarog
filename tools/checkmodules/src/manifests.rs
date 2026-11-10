use std::sync::Arc;

use bus::{AnyTx, Error, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler};
use controlplane::{Manifest, PROVIDER_SLOT, SOURCE_SLOT};
use lifecycle::{App, Context};

use crate::DeploymentProfile;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

struct NoopTransport;

#[async_trait::async_trait]
impl Transport for NoopTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        _contract: &EventContract,
        _payload: &[u8],
    ) -> Result<(), Error> {
        Ok(())
    }

    fn subscribe_tx(
        &self,
        _spec: SubscriptionSpec,
        _topic: &str,
        _version: u32,
        _history: Option<HistoryPolicy>,
        _handler: Arc<dyn TxHandler>,
    ) {
    }
}

/// One process's control plane: its own providers' manifests and the peers it dials.
pub struct ProcessControlPlane {
    pub process: &'static str,
    pub manifests: Vec<Manifest>,
    /// `Source.peer` of every `SOURCE_SLOT` contribution, in contribution order.
    pub dialled: Vec<String>,
}

/// Every process of `profile` with its control plane, in [`DeploymentProfile::processes`]
/// order.
///
/// Wires each process's module set through `register` → `init` over a lazy pool that
/// never connects and a no-op transport, then reads `controlplane::PROVIDER_SLOT` and
/// `controlplane::SOURCE_SLOT`. It builds and drops its own current-thread runtime, so
/// call it from a thread with no runtime.
pub fn control_planes(profile: &DeploymentProfile) -> anyhow::Result<Vec<ProcessControlPlane>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime: {e}"))?;
    rt.block_on(async {
        let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
        let mut out = Vec::new();
        for (process, modules) in profile.processes() {
            let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
                .map_err(|e| anyhow::anyhow!("{process}: build lazy pool: {e}"))?;
            let ctx = Arc::new(Context::with_db_and_transport(pool, Arc::new(NoopTransport)));
            let mut app = App::new(ctx.clone());
            for m in modules {
                app.add(m);
            }
            app.build().map_err(|e| anyhow::anyhow!("{process}: lifecycle build failed: {e:#}"))?;
            out.push(ProcessControlPlane {
                process,
                manifests: ctx
                    .contributions(PROVIDER_SLOT)
                    .iter()
                    .map(|provider| provider.manifest())
                    .collect(),
                dialled: ctx.contributions(SOURCE_SLOT).into_iter().map(|s| s.peer).collect(),
            });
        }
        anyhow::Ok(out)
    })
}

/// Every control-plane manifest the monolith serves, in contribution order. The monolith
/// hosts every module, so every provider is local and nothing is dialled.
pub fn monolith_manifests() -> anyhow::Result<Vec<Manifest>> {
    Ok(control_planes(&DeploymentProfile::Monolith)?
        .into_iter()
        .flat_map(|process| process.manifests)
        .collect())
}
