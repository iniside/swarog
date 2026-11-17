//! The `portal` control-plane provider: the dashboard elements no module owns. Every
//! value comes from `crate::fakes`.

use async_trait::async_trait;
use controlplane::{
    ContributionDecl, InvokeCall, InvokeOutcome, Manifest, PanelBody, PanelQuery, Point, Primitive, Provider, Role,
    SearchHit, SearchQuery, ServiceState, ServiceStatus, DEFAULT_PANEL_DEADLINE_MS,
};

use crate::fakes;

pub(crate) const MODULE: &str = "portal";

const TILE_LORE_SEATS: &str = "tile.lore-seats";
const TILE_LORE_STORE: &str = "tile.lore-store";
const TILE_MATCHES: &str = "tile.matches";
const ATTENTION_SERVICES: &str = "attention.services";
const ATTENTION_LORE: &str = "attention.lore";
const ATTENTION_SCHEDULER: &str = "attention.scheduler";
const HEALTH: &str = "health";

pub(crate) struct PortalProvider;

fn contribution(id: &str, point: Point, order: i32, title: &str, icon: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: icon.to_string(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
    }
}

fn health() -> PanelBody {
    let items = fakes::services();
    let running = items.iter().filter(|s| s.state == ServiceState::Running).count();
    PanelBody {
        meta: Some(format!("{running}/{} healthy · {}", items.len(), fakes::health_probe())),
        note: Some(fakes::history_window()),
        ..PanelBody::new(Primitive::ServiceStatus(ServiceStatus {
            items,
            columns: 2,
            value_label: Some("p99".to_string()),
        }))
    }
}

#[async_trait]
impl Provider for PortalProvider {
    fn manifest(&self) -> Manifest {
        Manifest {
            module: MODULE.to_string(),
            contributions: vec![
                contribution(TILE_LORE_SEATS, Point::DashboardTile, 20, "Lore seats", ""),
                contribution(TILE_LORE_STORE, Point::DashboardTile, 30, "Lore store used", ""),
                contribution(TILE_MATCHES, Point::DashboardTile, 80, "Matches in progress", ""),
                contribution(ATTENTION_SERVICES, Point::DashboardAttention, 10, "Service health", "alert"),
                contribution(ATTENTION_LORE, Point::DashboardAttention, 30, "Lore locks", "lock"),
                contribution(ATTENTION_SCHEDULER, Point::DashboardAttention, 50, "Scheduler runs", "clock"),
                contribution(HEALTH, Point::DashboardWide, 100, "Service health", ""),
            ],
            writes: false,
            ..Manifest::default()
        }
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        Ok(PanelBody::new(match req.contribution.as_str() {
            HEALTH => return Ok(health()),
            TILE_LORE_SEATS => Primitive::KpiTile(fakes::lore_seats()),
            TILE_LORE_STORE => Primitive::KpiTile(fakes::lore_store()),
            TILE_MATCHES => Primitive::KpiTile(fakes::matches_in_progress()),
            ATTENTION_SERVICES => Primitive::AttentionItems(fakes::attention_services()),
            ATTENTION_LORE => Primitive::AttentionItems(fakes::attention_lore()),
            ATTENTION_SCHEDULER => Primitive::AttentionItems(fakes::attention_scheduler()),
            other => return Err(opsapi::Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }))
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        Err(opsapi::Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
