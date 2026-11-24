//! The `portal` control-plane provider: the dashboard elements no module owns. Every
//! value comes from `crate::fakes`.

use async_trait::async_trait;
use controlplane::{
    ContributionDecl, InvokeCall, InvokeOutcome, Manifest, PanelBody, PanelQuery, Point, Primitive, Provider, Role,
    SearchHit, SearchQuery, ServiceState, ServiceStatus, DEFAULT_PANEL_DEADLINE_MS,
};
use controlplane::order;

use crate::fakes;

pub(crate) const MODULE: &str = "portal";

const ATTENTION_SERVICES: &str = "attention.services";
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
        readonly: None,
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
                contribution(
                    ATTENTION_SERVICES,
                    Point::DashboardAttention,
                    order::dashboard_attention::PORTAL_SERVICES,
                    "Service health",
                    "alert",
                ),
                contribution(HEALTH, Point::DashboardWide, order::dashboard_wide::PORTAL_HEALTH, "Service health", ""),
            ],
            writes: false,
            ..Manifest::default()
        }
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        Ok(PanelBody::new(match req.contribution.as_str() {
            HEALTH => return Ok(health()),
            ATTENTION_SERVICES => Primitive::AttentionItems(fakes::attention_services()),
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
