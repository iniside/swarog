//! The `opscatalog` op names this tool calls, one `const` each. A unit test reads this file
//! and fails on any name absent from `opscatalog::OPERATIONS`, so a typo never reaches a run.

pub const ACCOUNTS_LOGIN: &str = "accounts.login";
pub const ACCOUNTS_ME: &str = "accounts.me";
pub const ACCOUNTS_REGISTER: &str = "accounts.register";
pub const CHARACTERS_CREATE: &str = "characters.create";
pub const CHARACTERS_LIST: &str = "characters.list";
pub const FRIENDS_ACCEPT: &str = "friends.accept";
pub const FRIENDS_LIST: &str = "friends.list";
pub const FRIENDS_PENDING: &str = "friends.pending";
pub const FRIENDS_REQUEST: &str = "friends.request";
pub const GROUPS_CREATE: &str = "groups.create";
pub const GROUPS_DECIDE: &str = "groups.decide";
pub const GROUPS_INVITE: &str = "groups.invite";
pub const GROUPS_JOIN: &str = "groups.join";
pub const GROUPS_LIST_MINE: &str = "groups.listMine";
pub const GROUPS_MEMBERS: &str = "groups.members";
pub const GROUPS_PENDING: &str = "groups.pending";
pub const GROUPS_RESPOND: &str = "groups.respond";
pub const INVENTORY_GRANT: &str = "inventory.grant";
pub const INVENTORY_LIST_CHARACTER: &str = "inventory.listCharacter";
pub const INVENTORY_LIST_MINE: &str = "inventory.listMine";
pub const LEADERBOARD_TOP_SCORES: &str = "leaderboard.topScores";
pub const MATCH_REPORT: &str = "match.report";
pub const NOTIFICATIONS_LIST: &str = "notifications.list";
pub const NOTIFICATIONS_MARK_READ: &str = "notifications.markRead";
pub const STORAGE_GET: &str = "storage.get";
pub const STORAGE_LIST: &str = "storage.list";
pub const STORAGE_PUT: &str = "storage.put";
pub const WALLET_LIST_CURRENCIES: &str = "wallet.listCurrencies";
pub const WALLET_MY_BALANCES: &str = "wallet.myBalances";
