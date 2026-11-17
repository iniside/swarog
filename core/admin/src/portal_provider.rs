//! The `portal` control-plane provider: the dashboard elements no module owns. Every
//! value comes from `crate::fakes`.

use async_trait::async_trait;
use controlplane::{
    ContributionDecl, InvokeCall, InvokeOutcome, Manifest, PanelQuery, Point, Primitive, Provider, Role,
    SearchHit, SearchQuery, ServiceStatus, DEFAULT_PANEL_DEADLINE_MS,
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

#[async_trait]
impl Provider for PortalProvider {
    fn manifest(&self) -> Manifest {
        Manifest {
            module: MODULE.to_string(),
            contributions: vec![
                contribution(TILE_LORE_SEATS, Point::DashboardTile, 20, "Lore seats", "users"),
                contribution(TILE_LORE_STORE, Point::DashboardTile, 30, "Lore store used", "repo"),
                contribution(TILE_MATCHES, Point::DashboardTile, 80, "Matches in progress", "swords"),
                contribution(ATTENTION_SERVICES, Point::DashboardAttention, 10, "Service health", "alert"),
                contribution(ATTENTION_LORE, Point::DashboardAttention, 30, "Lore locks", "lock"),
                contribution(ATTENTION_SCHEDULER, Point::DashboardAttention, 50, "Scheduler runs", "clock"),
                contribution(HEALTH, Point::DashboardPanel, 100, "Service health", "server"),
            ],
            writes: false,
            ..Manifest::default()
        }
    }

    async fn panel(&self, req: PanelQuery) -> Result<Primitive, opsapi::Error> {
        Ok(match req.contribution.as_str() {
            TILE_LORE_SEATS => Primitive::KpiTile(fakes::lore_seats()),
            TILE_LORE_STORE => Primitive::KpiTile(fakes::lore_store()),
            TILE_MATCHES => Primitive::KpiTile(fakes::matches_in_progress()),
            ATTENTION_SERVICES => Primitive::AttentionItems(fakes::attention_services()),
            ATTENTION_LORE => Primitive::AttentionItems(fakes::attention_lore()),
            ATTENTION_SCHEDULER => Primitive::AttentionItems(fakes::attention_scheduler()),
            HEALTH => Primitive::ServiceStatus(ServiceStatus { items: fakes::services() }),
            other => return Err(opsapi::Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        })
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        Err(opsapi::Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
