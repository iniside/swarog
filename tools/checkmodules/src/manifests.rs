use std::sync::Arc;

use bus::{AnyTx, Error, EventContract, HistoryPolicy, SubscriptionSpec, Transport, TxHandler};
use controlplane::{Manifest, PROVIDER_SLOT};
use lifecycle::{App, Context};

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

/// Every control-plane manifest the monolith serves, in contribution order.
///
/// Wires [`crate::monolith_modules`] through `register` → `init` over a lazy pool that
/// never connects and a no-op transport, then reads `controlplane::PROVIDER_SLOT`. The
/// monolith hosts every module, so every provider is local and nothing is dialled. It
/// builds and drops its own current-thread runtime, so call it from a thread with no
/// runtime.
pub fn monolith_manifests() -> anyhow::Result<Vec<Manifest>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime: {e}"))?;
    rt.block_on(async {
        let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
        let pool = sqlx::postgres::PgPool::connect_lazy(&dsn)
            .map_err(|e| anyhow::anyhow!("build lazy pool: {e}"))?;
        let ctx = Arc::new(Context::with_db_and_transport(pool, Arc::new(NoopTransport)));
        let mut app = App::new(ctx.clone());
        for m in crate::monolith_modules() {
            app.add(m);
        }
        app.build()?;
        anyhow::Ok(
            ctx.contributions(PROVIDER_SLOT)
                .iter()
                .map(|provider| provider.manifest())
                .collect::<Vec<_>>(),
        )
    })
}
