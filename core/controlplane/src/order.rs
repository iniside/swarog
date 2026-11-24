//! The `order` of every point several modules contribute to, one constant per contribution,
//! named by its contributing module. Ties sort by module, then id.

/// `NavEntry` in the `platform` section.
pub mod platform_nav {
    pub const APIKEYS: i32 = 10;
    pub const MAIL: i32 = 20;
    pub const SCHEDULER: i32 = 30;
    pub const CONFIG: i32 = 40;
    pub const AUDIT: i32 = 50;
    pub const EVENTS: i32 = 60;
}

/// `Point::DashboardTile`.
pub mod dashboard_tile {
    pub const ACCOUNTS: i32 = 10;
    pub const PORTAL_LORE_SEATS: i32 = 20;
    pub const PORTAL_LORE_STORE: i32 = 30;
    pub const STORAGE: i32 = 40;
    pub const MAIL: i32 = 50;
    pub const EVENTS: i32 = 60;
    pub const WALLET: i32 = 70;
    pub const PORTAL_MATCHES: i32 = 80;
}

/// `Point::DashboardAttention`.
pub mod dashboard_attention {
    pub const PORTAL_SERVICES: i32 = 10;
    pub const EVENTS: i32 = 20;
    pub const PORTAL_LORE: i32 = 30;
    pub const MAIL: i32 = 40;
    pub const SCHEDULER: i32 = 50;
    pub const CHARACTERS_PURGES: i32 = 61;
    pub const WALLET_PURGES: i32 = 62;
    pub const INVENTORY_PURGES: i32 = 63;
    pub const FRIENDS_PURGES: i32 = 64;
    pub const GROUPS_PURGES: i32 = 65;
    pub const STORAGE_PURGES: i32 = 66;
    pub const NOTIFICATIONS_PURGES: i32 = 67;
}

/// `Point::DashboardPanel`.
pub mod dashboard_panel {
    pub const AUDIT: i32 = 10;
}

/// `Point::DashboardWide`.
pub mod dashboard_wide {
    pub const PORTAL_HEALTH: i32 = 100;
}

/// `Point::EntityTab` on the `player` kind.
pub mod player_tab {
    pub const ACCOUNTS_PROFILE: i32 = 10;
    pub const CHARACTERS: i32 = 20;
    pub const ACCOUNTS_IDENTITIES: i32 = 30;
    pub const WALLET: i32 = 30;
    pub const ACCOUNTS_SESSIONS: i32 = 40;
    pub const INVENTORY: i32 = 40;
    pub const FRIENDS: i32 = 50;
    pub const GROUPS: i32 = 60;
    pub const STORAGE: i32 = 70;
    pub const NOTIFICATIONS: i32 = 80;
}

/// `Point::EntitySummary` (the header facts) on the `player` kind.
pub mod player_fact {
    pub const ACCOUNTS_ID: i32 = 10;
    pub const ACCOUNTS_JOINED: i32 = 20;
    pub const CHARACTERS: i32 = 30;
    pub const LORE: i32 = 40;
    pub const WALLET: i32 = 50;
}

/// `Point::EntityListColumn` on the `player` kind.
pub mod player_column {
    pub const CHARACTERS: i32 = 10;
    pub const LORE: i32 = 20;
    pub const WALLET: i32 = 30;
}

/// `ActionPoint::HeaderAction` on the `player` kind.
pub mod player_header_action {
    pub const NOTIFICATIONS_NOTIFY: i32 = 20;
    pub const ACCOUNTS_SUSPEND: i32 = 30;
}

/// `ActionPoint::RowAction` on the `player` kind: one group per module.
pub mod player_row_action {
    pub const ACCOUNTS_SUSPEND: i32 = 20;
    pub const ACCOUNTS_DELETE: i32 = 30;
    pub const WALLET: i32 = 40;
    pub const INVENTORY: i32 = 50;
    pub const NOTIFICATIONS: i32 = 60;
    pub const STORAGE: i32 = 80;
}
