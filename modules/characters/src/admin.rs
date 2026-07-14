use async_trait::async_trait;
use opsapi::Error;

use crate::{internal, Service, Store, ADMIN_ITEM_ID, ADMIN_LABEL, ADMIN_SECTION};

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's page as `adminapi::ItemData` (same
    /// Section/Label the local `Item` carries), served on the edge as
    /// `admin.adminData` so a remote admin process renders it cross-process.
    async fn admin_data(&self) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store).await.map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
        })
    }
}

/// The live "Characters" block: a count KPI + a table of the newest 50 characters.
/// Reads only its own data and returns the admin's declarative widgets (the admin
/// owns the look). Async because it queries the store.
pub(crate) async fn admin_content(store: &Store) -> anyhow::Result<adminapi::Content> {
    let n = store.count().await?;
    let rows = store.list_all(50).await?;

    let mut table = adminapi::Table {
        columns: vec!["NAME".into(), "CLASS".into(), "PLAYER".into(), "CREATED".into()],
        rows: Vec::with_capacity(rows.len()),
    };
    for c in rows {
        table.rows.push(vec![
            adminapi::Cell::text(&c.name),
            adminapi::Cell {
                text: c.class,
                badge: "blue".into(),
                ..Default::default()
            },
            adminapi::Cell::mono(&c.player_id),
            adminapi::Cell::text(&c.created_at),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Characters".into(),
            value: n.to_string(),
            sub: String::new(),
        }],
        table: Some(table),
        form: None,
    })
}
