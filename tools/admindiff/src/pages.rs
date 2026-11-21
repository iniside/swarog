//! Which mockup page compares with which portal route. A page is named by its mockup file
//! stem (`UILayout/redesign/<page>.html`); the file's `PAGES.boot("<key>")` names the
//! `P.<key>` builder the harness renders. An entry that names a tab of a `FirstRow` page is
//! its own target, `<page>.<tab>`, with its own crawl output and accept file. Every other tab
//! the mockup draws on a `FirstRow` page, besides the one its untabbed entry renders
//! selected, is in [`EXCLUDED_TABS`]. An entry that names a menu is its own target too,
//! `<host>.menu.<menu>`: the first menu of that kind its host page draws, opened on both
//! sides. Menus are inventoried on every compared page or tab ([`Page::is_compared_host`]),
//! on both sides: every menu the mockup draws there has such an entry and every menu its
//! registry defines has one or is in [`EXCLUDED_MOCK_MENUS`] ([`validate_menus`]); every
//! menu kind the portal draws there has one or is in [`EXCLUDED_MENUS`] (`crawl`). An entry that names a modal is its own target,
//! `<host>.modal.<modal>`: the harness calls the mockup's named opener with the entry's
//! arguments, and the crawl opens the first opener of the entry's portal action its host
//! draws. A modal's host is a page, a tab (`<page>.<tab>.modal.<modal>`) or a menu target
//! (`<page>[.<tab>].menu.<menu>.modal.<modal>`, a modal a row menu opens). Modals are
//! inventoried on every crawled page and tab entry that is not pending, in every menu target's
//! fetched menu, and in every excluded menu's ([`excluded_menu_id`]): every modal action the
//! portal draws in them (an `hx-get` to `/admin/_action/…`) has such an entry, is in
//! [`EXCLUDED_MODALS`] or is the action of an [`EXCLUDED_MOCK_MODALS`] claim of that host
//! (`crawl`); on the mockup side an entry's opener must append a `.scrim`.
//! The mockup's modals are inventoried from its renders of every page, tab and menu target that
//! is not pending: every `onclick` body drawn there runs in the harness, and each `.scrim` it
//! appends is a modal of the `PAGES` function that appended it, or of the body when no `PAGES`
//! call did ([`MockOpener`]); each is the shell palette ([`PALETTE_ONCLICK`]), a modal entry of
//! that host or in [`EXCLUDED_MOCK_MODALS`], which names what the portal draws instead
//! ([`PortalModal`], a claim the crawl checks), and an `EXCLUDED_MODALS` reason that the
//! mockup has no opener ([`ModalWhy`]) is held to that inventory ([`validate_mock_modals`]).
//! A page or tab entry
//! can force the mockup's ok state on its render ([`Page::force`]), so a panel the mockup draws
//! only down, stale or denied is compared in its ok shape, and its menu entries open their menu
//! on that forced render; the portal side draws its real state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use anyhow::{bail, Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Ported: the portal route the crawl fetches and the diff compares.
    Mapped(&'static str),
    /// Ported entity page: the crawl follows the first `tr.clickable` `data-href` of the
    /// named `Mapped` list page, whose entity ids a fleet mints at random.
    FirstRow(&'static str),
    /// Not ported yet. `crawl` prints it on every run and `diff` refuses it.
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub page: &'static str,
    pub route: Route,
    /// The harness's `location.search`; an entity page's fixed entity lives here.
    pub search: &'static str,
    /// A tab of a `FirstRow` page: both sides add `tab=<id>` to their query, and a crawl
    /// or render that does not select it fails.
    pub tab: Option<&'static str>,
    /// A menu drawn on the page this entry's route, search and tab name, its host: the crawl
    /// fetches the first such menu the portal draws and the harness opens the first the
    /// mockup draws; a host that draws none fails.
    pub menu: Option<Menu>,
    /// A modal opened from its host ([`Page::host`]): the page this entry's route, search and tab
    /// name, or, when [`Page::menu`] is set too, that menu target. The harness calls its mockup
    /// opener, and the crawl fetches the first opener of its portal action the host draws.
    pub modal: Option<Modal>,
    /// Whether the harness forces the mockup's ok state on this entry's render: it sets
    /// `UI.state.force = "ok"`, as the mockup's state picker does, before the page renders.
    /// Only a page or tab entry forces; a menu entry of it opens its menu on the forced render.
    /// A `state=` parameter in [`Page::search`] is refused, so this field is the one way to
    /// force. The crawl ignores it: the portal draws its real state.
    pub force: bool,
}

/// A row or header menu: the mockup opens it with `UI.openMenu(event,'<mock>','<arg>')`, the
/// portal serves it at `/admin/_menu/<kind>/<id>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Menu {
    pub mock: &'static str,
    pub kind: &'static str,
}

/// A modal: the mockup opens it with `PAGES.<opener>(<args>)`, which appends a `.scrim` to the
/// body; the portal serves it at `/admin/_action/<module>/<action>`, with the `?entity=` and
/// `?item=` its host's opener carries, since a fleet mints those ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modal {
    pub id: &'static str,
    pub opener: &'static str,
    pub args: &'static [&'static str],
    pub module: &'static str,
    pub action: &'static str,
}

impl Modal {
    /// `<module>/<action>`, as the portal's action URL names it.
    pub fn action_path(&self) -> String {
        format!("{}/{}", self.module, self.action)
    }
}

impl Page {
    /// The target's name: `<page>`, `<page>.<tab>` for a tab entry, and `.menu.<menu>` or
    /// `.modal.<modal>` after either for a menu or modal entry. It names the crawl's output,
    /// the accept file and the `--page` argument. A tab id holds no `.`, so a menu or modal id
    /// is never a tab's.
    pub fn id(&self) -> String {
        let mut id = self.page.to_string();
        if let Some(tab) = self.tab {
            id = format!("{id}.{tab}");
        }
        if let Some(menu) = self.menu {
            id = format!("{id}.menu.{}", menu.mock);
        }
        if let Some(modal) = self.modal {
            id = format!("{id}.modal.{}", modal.id);
        }
        id
    }

    /// What a menu or modal entry's target is drawn on, unforced, as a menu or modal entry
    /// always is: for a modal entry this entry without its modal, a page or a menu target; else
    /// this entry without its menu, a page, whose host entry ([`Page::hosts`]) may force.
    pub fn host(&self) -> Page {
        match self.modal {
            Some(_) => Page { modal: None, force: false, ..*self },
            None => Page { menu: None, force: false, ..*self },
        }
    }

    /// Whether this entry is a menu target: a menu and no modal.
    pub fn is_menu(&self) -> bool {
        self.menu.is_some() && self.modal.is_none()
    }

    /// Whether `self` is a page or tab entry that hosts `host`, forced or not.
    pub fn hosts(&self, host: &Page) -> bool {
        self.is_page() && self.host() == *host
    }

    /// Whether this entry is a compared host: a page or tab entry that is not pending. The
    /// crawl holds every menu kind and modal action such a host draws to the lists, and a menu
    /// or modal exclusion names one, a menu target of one or an excluded menu of one as its
    /// host ([`compared_hosts`]).
    pub fn is_compared_host(&self) -> bool {
        self.is_page() && self.route != Route::Pending
    }

    /// Whether this entry is a compared target that hosts menus or modals: a compared host, or
    /// a menu target of one. Both sides inventory the modal openers each such target draws.
    pub fn is_compared_target(&self) -> bool {
        self.modal.is_none() && self.host().is_compared_host()
    }

    /// Whether this entry is a page or tab target, neither a menu nor a modal.
    pub fn is_page(&self) -> bool {
        self.menu.is_none() && self.modal.is_none()
    }

    /// The harness's `location.search`: [`Page::search`] plus the tab's `tab=<id>`.
    pub fn mock_search(&self) -> String {
        match self.tab {
            Some(tab) => with_tab(self.search, tab),
            None => self.search.to_string(),
        }
    }
}

/// The id of every [`Page::is_compared_host`] entry of `page_map`, once each, in map order.
pub fn compared_hosts(page_map: &[Page]) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for p in page_map.iter().filter(|p| p.is_compared_host()) {
        if !hosts.contains(&p.id()) {
            hosts.push(p.id());
        }
    }
    hosts
}

/// `url` with `tab=<tab>` added to its query. A tab id is `[a-z0-9_-]+` ([`validate_lists`]),
/// so it needs no encoding.
pub fn with_tab(url: &str, tab: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}tab={tab}")
}

/// Whether the query of `search` (`?a=1&b`) names parameter `name`, with or without a value.
fn names_param(search: &str, name: &str) -> bool {
    let query = search.strip_prefix('?').unwrap_or(search);
    query.split('&').any(|pair| pair.split('=').next() == Some(name))
}

fn is_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A portal module or action id: `[a-z0-9._-]+`, as `core.events` and `poison.skip` are.
fn is_action_id(id: &str) -> bool {
    is_id(&id.replace('.', "-"))
}

/// A `<module>/<action>` path of two [`is_action_id`] ids.
fn is_action_path(path: &str) -> bool {
    path.split_once('/').is_some_and(|(module, action)| is_action_id(module) && is_action_id(action))
}

/// A JavaScript identifier the harness can look up as `PAGES[<opener>]`.
fn is_opener(name: &str) -> bool {
    let mut bytes = name.bytes();
    let first = bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'$');
    first && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// The user row menu (`registry.js` `UI.defineMenu("user")`), the portal's `player` row actions.
const USER_MENU: Menu = Menu { mock: "user", kind: "player" };

pub const PAGE_MAP: &[Page] = &[
    Page { page: "index", route: Route::Mapped("/admin"), search: "", tab: None, menu: None, modal: None, force: false },
    Page {
        page: "events",
        route: Route::Mapped("/admin/p/core.events/events"),
        search: "",
        tab: None,
        menu: None,
        modal: None,
        force: false,
    },
    Page {
        page: "events",
        route: Route::Mapped("/admin/p/core.events/events"),
        search: "",
        tab: None,
        menu: None,
        modal: Some(Modal { id: "skip-poison", opener: "skipPoison", args: &[], module: "core.events", action: "poison.skip" }),
        force: false,
    },
    Page {
        page: "players",
        route: Route::Mapped("/admin/p/accounts/players"),
        search: "",
        tab: None,
        menu: None,
        modal: None,
        force: false,
    },
    Page {
        page: "players",
        route: Route::Mapped("/admin/p/accounts/players"),
        search: "",
        tab: None,
        menu: Some(USER_MENU),
        modal: None,
        force: false,
    },
    Page { page: "player", route: Route::FirstRow("players"), search: "?u=aiko.tanaka", tab: None, menu: None, modal: None, force: false },
    Page {
        page: "player",
        route: Route::FirstRow("players"),
        search: "?u=aiko.tanaka",
        tab: None,
        menu: Some(USER_MENU),
        modal: None,
        force: false,
    },
    Page { page: "apikeys", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false },
    Page { page: "mail", route: Route::Mapped("/admin/p/mail/outbox"), search: "", tab: None, menu: None, modal: None, force: false },
    Page { page: "scheduler", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false },
    Page { page: "config", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false },
    Page { page: "audit", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false },
    Page { page: "economy", route: Route::Pending, search: "", tab: None, menu: None, modal: None, force: false },
];

const NO_LORE: &str = "no Lore module exists yet";

pub const EXCLUDED: &[(&str, &str)] = &[
    ("states", "Declared deviation D3: no states page; the golden render test pins every panel state"),
    ("catalog", "Declared deviation D3: no catalog page; the golden render test pins every primitive"),
    ("lore-repos", NO_LORE),
    ("lore-repo", NO_LORE),
    ("lore-permissions", NO_LORE),
    ("lore-locks", NO_LORE),
    ("lore-server", NO_LORE),
    ("lore-events", NO_LORE),
    ("lore-billing", NO_LORE),
];

/// Tabs the mockup draws on a `FirstRow` page that no `PAGE_MAP` tab entry diffs, as
/// `<page>.<tab>` with the reason. [`validate_tabs`] holds it to the mockup render.
pub const EXCLUDED_TABS: &[(&str, &str)] = &[
    ("player.characters", "not ported yet: the Characters tab arrives with the characters port (Step 14)"),
    ("player.wallet", "not ported yet: the Wallet tab arrives with the wallet port (Step 15)"),
    ("player.inventory", "not ported yet: the Inventory tab arrives with the inventory port (Step 16)"),
    ("player.friends", "not ported yet: the Friends tab arrives with the friends port (Step 17)"),
    ("player.groups", "not ported yet: the Groups tab arrives with the groups port (Step 18)"),
    ("player.saves", "not ported yet: the Cloud saves tab arrives with the storage port (Step 19)"),
    ("player.inbox", "not ported yet: the Inbox tab arrives with the notifications port (Step 20)"),
    ("player.lore", NO_LORE),
];

/// Menus the portal draws on a compared page or tab ([`Page::is_compared_host`]) that no
/// `PAGE_MAP` menu entry diffs, as `(<host id>, <kind>, <reason>)`, the host a page or tab
/// entry's id (`<page>`, `<page>.<tab>`). The crawl still fetches the first of each and fails on
/// a non-2xx answer, and fails on an entry whose host does not draw it (a stale entry).
pub const EXCLUDED_MENUS: &[(&str, &str, &str)] = &[(
    "events",
    "subscription",
    "the mockup draws no subscription menu: its events row button only toasts (pages-platform.js P.events)",
)];

/// Modal actions the portal draws on a compared page or tab, or in a menu target's or excluded
/// menu's body, that no `PAGE_MAP` modal entry diffs, as `(<host id>, <module>/<action>, <why>)`:
/// the host is a page or tab entry's id (`<page>`, `<page>.<tab>`), a `PAGE_MAP` menu
/// target's id or an [`EXCLUDED_MENUS`] menu's [`excluded_menu_id`]. [`validate_excluded_modals`] holds it to `PAGE_MAP` and
/// `EXCLUDED_MENUS`; the crawl still fetches the first of each and fails on an answer that is no
/// modal, and fails on an entry whose host does not draw it (a stale entry). Its reason, that
/// the mockup has no opener, is a [`ModalWhy`], which [`validate_mock_modals`] holds to the
/// mockup render.
pub const EXCLUDED_MODALS: &[(&str, &str, ModalWhy)] = &[
    (
        "mail",
        "mail/test-send",
        ModalWhy { why: "the mockup's Test send button has no onclick, so the mockup has no modal; its cancelMail opens mail/cancel (pages-platform.js P.mail)" },
    ),
    (
        "events.excluded-menu.subscription",
        "core.events/sub.pause",
        ModalWhy { why: "the mockup draws no subscription menu and no pause opener: its events row button only toasts (pages-platform.js P.events)" },
    ),
];

/// Why an [`EXCLUDED_MODALS`] action is not diffed: the mockup draws no opener of the action on
/// the host, or, for an [`excluded_menu_id`] host, on its page, a mockup fact checked against the
/// mockup render: every modal opener the host's mockup render draws is the shell palette, a
/// `PAGE_MAP` modal entry or an [`EXCLUDED_MOCK_MODALS`] entry of another action there
/// ([`validate_mock_modals`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModalWhy<'a> {
    pub why: &'a str,
}

/// The host id of the menu of `kind` an [`EXCLUDED_MENUS`] entry excludes on page or tab `host`, as an
/// [`EXCLUDED_MODALS`] entry names it: no `PAGE_MAP` id has the `.excluded-menu.` segment.
pub fn excluded_menu_id(host: &str, kind: &str) -> String {
    format!("{host}.excluded-menu.{kind}")
}

/// Menus the mockup registry defines (`UI.defineMenu`) that no `PAGE_MAP` menu entry diffs,
/// with the reason. [`validate_menus`] holds it to the registry.
pub const EXCLUDED_MOCK_MENUS: &[(&str, &str)] = &[
    ("apikey", "drawn only on apikeys, which is pending"),
    ("repo", NO_LORE),
    ("lock", NO_LORE),
];

/// The `PAGES` keys other than `tab` that a compared page's `.tab` buttons assign when they
/// are not tabs of the page: a probed `.tab` whose body assigns no `PAGES.tab` fails unless it
/// assigns one of these ([`crate::mock::probe`]), and a key no probed `.tab` assigns is stale. A
/// key is added only when a compared page draws a `.tab` that is not a tab. Lore-repo's
/// `PAGES.repoTab` (`UILayout/redesign/assets/pages-lore.js`, `P.loreRepo`) joins when the lore
/// pages are compared; lore-repo is excluded with `NO_LORE`, so no compared page draws one.
/// Each key comes with the reason its `.tab` is not a tab of the page.
pub const NON_TAB_KEYS: &[(&str, &str)] = &[];

/// What opened a `.scrim` an `onclick` body of a mockup render appended in the harness: the
/// `PAGES` function whose call by the body appended it, or the body itself, verbatim, when it was
/// appended outside every such call (`REG.openSearch()`, an inline `UI.confirm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MockOpener<S> {
    Pages(S),
    Onclick(S),
}

impl<S: AsRef<str>> MockOpener<S> {
    /// The same opener over borrowed text.
    pub fn as_str(&self) -> MockOpener<&str> {
        match self {
            MockOpener::Pages(name) => MockOpener::Pages(name.as_ref()),
            MockOpener::Onclick(body) => MockOpener::Onclick(body.as_ref()),
        }
    }
}

impl<S: AsRef<str>> fmt::Display for MockOpener<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            MockOpener::Pages(name) => write!(f, "PAGES.{name}"),
            MockOpener::Onclick(body) => write!(f, "onclick {body:?}"),
        }
    }
}

/// What the portal draws on its host for a mockup modal opener [`EXCLUDED_MOCK_MODALS`] lists. The
/// crawl checks each claim on the host it fetched; a host it did not fetch (skipped or failed)
/// checks none, and the crawl prints the claim UNCHECKED. A claim whose host is no compared target
/// fails [`validate_mock_modal_hosts`], before the crawl fetches anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalModal<'a> {
    /// The portal modal action `<module>/<action>`, served at `/admin/_action/<module>/<action>`:
    /// the host draws an opener of it, whose modal the crawl fetches.
    Action(&'a str),
    /// The portal action `<module>/<action>` drawn disabled as not implemented, with no modal: the
    /// host draws a disabled control (`button[disabled]`, `.menu-item.disabled`) whose
    /// `data-refused-action` is the action and `data-refusal` is `not-implemented`, none of the
    /// action with another `data-refusal`, and no opener of the action.
    Disabled(&'a str),
    /// The portal modal action `<module>/<action>`, whose opener the portal draws only on data the
    /// dev fleet does not seed, as the reason says: the host draws no opener of it. That the
    /// portal has the action is not checked, as the crawl sees no opener of it.
    Unseeded(&'a str),
    /// Nothing: no portal module draws it, as the reason says. Not checked: it names no action
    /// the crawl could look for.
    Absent,
}

impl<'a> PortalModal<'a> {
    /// The portal action `<module>/<action>` the claim names; none for [`PortalModal::Absent`].
    pub fn action(&self) -> Option<&'a str> {
        match *self {
            PortalModal::Action(a) | PortalModal::Disabled(a) | PortalModal::Unseeded(a) => Some(a),
            PortalModal::Absent => None,
        }
    }
}

/// A mockup modal opener drawn on a compared page, tab or menu target that no `PAGE_MAP` modal
/// entry of that host diffs: its host id, the opener, what the portal opens instead and why it is
/// not diffed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockModalExclusion<'a> {
    pub host: &'a str,
    pub opener: MockOpener<&'a str>,
    pub portal: PortalModal<'a>,
    pub why: &'a str,
}

/// The mockup topbar's search trigger (`registry.js` `REG.openSearch`), drawn on every page: it
/// opens the Ctrl-K palette, a `.scrim` it appends outside every `PAGES` call. It is the shell's
/// palette on every compared host that draws it, so no host lists it: the portal draws no modal
/// action for it but one hidden `.scrim[data-palette]` in the shell that `admin.js` opens, which
/// the crawl requires on every page it fetches.
pub const PALETTE_ONCLICK: &str = "REG.openSearch()";

const fn mock_modal(host: &'static str, opener: MockOpener<&'static str>, portal: PortalModal<'static>, why: &'static str) -> MockModalExclusion<'static> {
    MockModalExclusion { host, opener, portal, why }
}

/// Modal openers the mockup draws on a compared page, tab or menu target ([`MockOpener`]) that no
/// `PAGE_MAP` modal entry of that host diffs. [`validate_mock_modals`] holds it to the mockup
/// render.
pub const EXCLUDED_MOCK_MODALS: &[MockModalExclusion] = &[
    mock_modal("players.menu.user", MockOpener::Pages("suspend"), SUSPEND, NO_SUSPEND),
    mock_modal("players.menu.user", MockOpener::Pages("deleteAccount"), DELETE_ACCOUNT, NO_DELETE),
    mock_modal("players.menu.user", MockOpener::Pages("changeRole"), PortalModal::Absent, NO_LORE),
    mock_modal("players.menu.user", MockOpener::Pages("releaseAll"), PortalModal::Absent, NO_LORE),
    mock_modal("player", MockOpener::Pages("changeRole"), PortalModal::Absent, NO_LORE),
    mock_modal("player", MockOpener::Pages("suspend"), PortalModal::Disabled("accounts/suspend-header"), NO_SUSPEND),
    mock_modal("player.menu.user", MockOpener::Pages("suspend"), SUSPEND, NO_SUSPEND),
    mock_modal("player.menu.user", MockOpener::Pages("deleteAccount"), DELETE_ACCOUNT, NO_DELETE),
    mock_modal("player.menu.user", MockOpener::Pages("changeRole"), PortalModal::Absent, NO_LORE),
    mock_modal("player.menu.user", MockOpener::Pages("releaseAll"), PortalModal::Absent, NO_LORE),
    mock_modal(
        "mail",
        MockOpener::Pages("cancelMail"),
        PortalModal::Unseeded("mail/cancel"),
        "mail.modal.cancel left PAGE_MAP: the portal draws the cancel opener only on pending or retrying mail, which the dev fleet does not seed; Step 24b3 adds it back",
    ),
];

/// Accounts' row menu suspend and delete-account actions (`modules/accounts/src/admin.rs`).
const SUSPEND: PortalModal = PortalModal::Disabled("accounts/suspend");
const DELETE_ACCOUNT: PortalModal = PortalModal::Disabled("accounts/delete-account");

const NO_SUSPEND: &str = "no suspension exists: suspend is new domain behaviour the plan excludes, so the portal has no suspend modal";
const NO_DELETE: &str =
    "deletion is the player's own two-step flow with no operator path, so the portal has no delete-account modal";

/// A page map with the exclusion lists that complete it: what the crawl, the mockup render
/// checks and the diff read, so a caller that passes its own map passes its own lists too.
/// Problems name the production lists either way.
#[derive(Clone, Copy, Debug)]
pub struct Lists<'a> {
    pub page_map: &'a [Page],
    pub excluded: &'a [(&'a str, &'a str)],
    pub excluded_tabs: &'a [(&'a str, &'a str)],
    pub excluded_menus: &'a [(&'a str, &'a str, &'a str)],
    pub excluded_modals: &'a [(&'a str, &'a str, ModalWhy<'a>)],
    pub excluded_mock_menus: &'a [(&'a str, &'a str)],
    pub excluded_mock_modals: &'a [MockModalExclusion<'a>],
    pub non_tab_keys: &'a [(&'a str, &'a str)],
    /// The `onclick` body of the shell palette's mockup opener, [`PALETTE_ONCLICK`] in production.
    pub palette_onclick: &'a str,
}

/// The production lists the CLI runs on.
pub const LISTS: Lists<'static> = Lists {
    page_map: PAGE_MAP,
    excluded: EXCLUDED,
    excluded_tabs: EXCLUDED_TABS,
    excluded_menus: EXCLUDED_MENUS,
    excluded_modals: EXCLUDED_MODALS,
    excluded_mock_menus: EXCLUDED_MOCK_MENUS,
    excluded_mock_modals: EXCLUDED_MOCK_MODALS,
    non_tab_keys: NON_TAB_KEYS,
    palette_onclick: PALETTE_ONCLICK,
};

/// The tabs a mockup render draws, in order, and the one it draws selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawnTabs {
    pub tabs: Vec<String>,
    pub selected: Option<String>,
}

/// Every mockup page under `redesign` with its `P.<key>`, read from the `PAGES.boot` call.
pub fn mockup_pages(redesign: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let dir = std::fs::read_dir(redesign).with_context(|| format!("read {}", redesign.display()))?;
    for entry in dir {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let src = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let key = boot_key(&src).with_context(|| format!("{}: no PAGES.boot(\"<key>\") call", path.display()))?;
        out.insert(stem, key);
    }
    Ok(out)
}

fn boot_key(src: &str) -> Option<String> {
    let at = src.find("PAGES.boot(\"")? + "PAGES.boot(\"".len();
    let len = src[at..].find('"')?;
    Some(src[at..at + len].to_string())
}

/// Checks `page_map` and `excluded`, which its problems name `PAGE_MAP` and `EXCLUDED`, against
/// the mockup and each other: every listed page exists, no target is listed twice or in both
/// lists, every mockup page is listed,
/// every `FirstRow` names a `Mapped` page, every tab entry has a `[a-z0-9_-]+` id and
/// an untabbed `FirstRow` entry of its page with the same route, every menu entry has
/// `[a-z0-9_-]+` ids and a host entry ([`Page::host`]) of a page that is not pending, and every
/// modal entry has a `[a-z0-9_-]+` id, a JavaScript identifier for its opener, `[a-z0-9._-]+`
/// module and action ids and such a host entry, or, when it names a menu, that menu target as
/// a `PAGE_MAP` menu entry. No search names a `state`
/// parameter, and only a page or tab entry forces the ok state.
pub(crate) fn validate_lists(mockup: &BTreeMap<String, String>, page_map: &[Page], excluded: &[(&str, &str)]) -> Result<()> {
    let mut problems = Vec::new();
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    let listed =
        page_map.iter().map(|p| (p.id(), p.page, "PAGE_MAP")).chain(excluded.iter().map(|(p, _)| (p.to_string(), *p, "EXCLUDED")));
    for (id, page, list) in listed {
        if let Some(first) = seen.insert(id.clone(), list) {
            problems.push(format!("{id} is listed in {first} and again in {list}"));
        }
        if !mockup.contains_key(page) {
            problems.push(format!("{list} lists {id}, which is not a mockup page"));
        }
    }
    for page in mockup.keys() {
        if !seen.contains_key(page.as_str()) && !page_map.iter().any(|p| p.page == page) {
            problems.push(format!("mockup page {page} is in neither PAGE_MAP nor EXCLUDED"));
        }
    }
    for p in page_map {
        if let Route::FirstRow(list) = p.route {
            if !page_map.iter().any(|q| q.page == list && matches!(q.route, Route::Mapped(_))) {
                problems.push(format!("PAGE_MAP {} follows {list}, which is not a Mapped page", p.page));
            }
        }
        if let Some(menu) = p.menu.filter(|_| p.is_menu()) {
            let id = p.id();
            for (what, name) in [("menu", menu.mock), ("kind", menu.kind)] {
                if !is_id(name) {
                    problems.push(format!("PAGE_MAP {id} names {what} {name:?}, which is not a [a-z0-9_-]+ id"));
                }
            }
            let host = p.host();
            if p.route == Route::Pending {
                problems.push(format!("PAGE_MAP {id} names a menu of {}, which is pending", host.id()));
            } else if !page_map.iter().any(|q| q.hosts(&host)) {
                problems.push(format!("PAGE_MAP {id} is a menu of {}, which has no entry with the same route", host.id()));
            }
        }
        if let Some(modal) = p.modal {
            let id = p.id();
            if !is_id(modal.id) {
                problems.push(format!("PAGE_MAP {id} names modal {:?}, which is not a [a-z0-9_-]+ id", modal.id));
            }
            if !is_opener(modal.opener) {
                problems.push(format!("PAGE_MAP {id} names opener {:?}, which is not a JavaScript identifier", modal.opener));
            }
            for (what, name) in [("module", modal.module), ("action", modal.action)] {
                if !is_action_id(name) {
                    problems.push(format!("PAGE_MAP {id} names {what} {name:?}, which is not a [a-z0-9._-]+ id"));
                }
            }
            let host = p.host();
            if p.route == Route::Pending {
                problems.push(format!("PAGE_MAP {id} names a modal of {}, which is pending", host.id()));
            } else if host.menu.is_some() {
                if !page_map.iter().any(|q| q.is_menu() && *q == host) {
                    problems.push(format!("PAGE_MAP {id} is a modal of menu {}, which is no PAGE_MAP menu entry", host.id()));
                }
            } else if !page_map.iter().any(|q| q.hosts(&host)) {
                problems.push(format!("PAGE_MAP {id} is a modal of {}, which has no entry with the same route", host.id()));
            }
        }
        if names_param(p.search, "state") {
            problems.push(format!("PAGE_MAP {} carries state in its search; a forced state is the force field", p.id()));
        }
        if p.force && !p.is_page() {
            problems.push(format!("PAGE_MAP {} forces the ok state, which only a page or tab entry may", p.id()));
        }
        let Some(tab) = p.tab else { continue };
        let id = p.id();
        if !is_id(tab) {
            problems.push(format!("PAGE_MAP {id} names tab {tab:?}, which is not a [a-z0-9_-]+ id"));
        }
        if !matches!(p.route, Route::FirstRow(_)) {
            problems.push(format!("PAGE_MAP {id} names a tab, which only a FirstRow page has"));
        } else if !page_map.iter().any(|q| q.page == p.page && q.tab.is_none() && q.is_page() && q.route == p.route) {
            problems.push(format!("PAGE_MAP {id} is a tab of {}, which has no untabbed entry with the same route", p.page));
        }
    }
    for (page, why) in excluded {
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED {page} has no reason"));
        }
    }
    if !problems.is_empty() {
        bail!("page map drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Checks the tabs the mockup draws on each untabbed `FirstRow` page (`drawn`, by page)
/// against `page_map` and `excluded_tabs`, which its problems name `PAGE_MAP` and
/// `EXCLUDED_TABS`: every drawn tab but the selected one, which the untabbed entry diffs, is a
/// tab entry or excluded, not both; an exclusion names a drawn, unselected tab, once, with a
/// reason.
pub fn validate_tabs(drawn: &[(&str, DrawnTabs)], page_map: &[Page], excluded_tabs: &[(&str, &str)]) -> Result<()> {
    let mut problems = Vec::new();
    for (page, d) in drawn {
        for tab in d.tabs.iter().filter(|t| d.selected.as_ref() != Some(*t)) {
            let id = format!("{page}.{tab}");
            let mapped = page_map.iter().any(|p| p.page == *page && p.tab == Some(tab.as_str()) && p.is_page());
            let excluded = excluded_tabs.iter().any(|(t, _)| *t == id);
            match (mapped, excluded) {
                (false, false) => problems.push(format!("mockup tab {id} is in neither PAGE_MAP nor EXCLUDED_TABS")),
                (true, true) => problems.push(format!("{id} is listed in PAGE_MAP and again in EXCLUDED_TABS")),
                _ => {}
            }
        }
    }
    // The page drawing `id`'s tab, and whether it draws that tab selected.
    let drawn_as = |id: &str| {
        drawn.iter().find_map(|(page, d)| {
            let tab = id.strip_prefix(page)?.strip_prefix('.')?;
            d.tabs.iter().any(|t| t == tab).then_some((*page, d.selected.as_deref() == Some(tab)))
        })
    };
    // A mapped tab the mockup stops drawing: without this, a markup change that hides every
    // tab from `drawn` would pass once no tab is left in EXCLUDED_TABS.
    for p in page_map.iter().filter(|p| p.is_page()) {
        let Some(tab) = p.tab else { continue };
        let id = format!("{}.{tab}", p.page);
        match drawn_as(&id) {
            None => problems.push(format!("PAGE_MAP maps {id}, which the mockup does not draw")),
            Some((page, true)) => problems.push(format!("PAGE_MAP maps {id}, the tab the untabbed {page} entry diffs")),
            Some((_, false)) => {}
        }
    }
    let mut seen = BTreeSet::new();
    for (id, why) in excluded_tabs {
        if !seen.insert(*id) {
            problems.push(format!("{id} is listed in EXCLUDED_TABS and again in EXCLUDED_TABS"));
        }
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED_TABS {id} has no reason"));
        }
        match drawn_as(id) {
            None => problems.push(format!("EXCLUDED_TABS lists {id}, which the mockup does not draw")),
            Some((page, true)) => problems.push(format!("EXCLUDED_TABS lists {id}, the tab the untabbed {page} entry diffs")),
            Some((_, false)) => {}
        }
    }
    if !problems.is_empty() {
        bail!("tab drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Checks the mockup's menus against `page_map` and `excluded_mock_menus`, which its problems
/// name `PAGE_MAP` and `EXCLUDED_MOCK_MENUS`. `defined` is every menu id the registry defines
/// (`UI.defineMenu`): each is a menu entry's or excluded, not both, and every menu entry and
/// exclusion names a defined id; an exclusion is listed once, with a reason. `drawn` is each
/// compared host's id ([`Page::is_compared_host`]), a page or a tab, with the `UI.openMenu`
/// ids its render draws: each is a menu entry of that host. A menu entry its host does not draw fails its own render
/// ([`crate::mock::Probe::menu_arg`]).
pub fn validate_menus(
    drawn: &[(String, Vec<String>)],
    defined: &[String],
    page_map: &[Page],
    excluded_mock_menus: &[(&str, &str)],
) -> Result<()> {
    let mut problems = Vec::new();
    for (host, menus) in drawn {
        for menu in menus {
            if !page_map.iter().any(|p| p.is_menu() && p.menu.is_some_and(|m| m.mock == menu) && p.host().id() == *host) {
                problems.push(format!("mockup menu {host}.menu.{menu} is not a PAGE_MAP menu entry"));
            }
        }
    }
    for id in defined {
        let mapped = page_map.iter().any(|p| p.is_menu() && p.menu.is_some_and(|m| m.mock == id));
        let excluded = excluded_mock_menus.iter().any(|(m, _)| m == id);
        match (mapped, excluded) {
            (false, false) => problems.push(format!("mockup menu {id} is in neither PAGE_MAP nor EXCLUDED_MOCK_MENUS")),
            (true, true) => problems.push(format!("mockup menu {id} is listed in PAGE_MAP and again in EXCLUDED_MOCK_MENUS")),
            _ => {}
        }
    }
    let is_defined = |id: &str| defined.iter().any(|d| d == id);
    for p in page_map {
        if let Some(menu) = p.menu.filter(|m| !is_defined(m.mock)) {
            problems.push(format!("PAGE_MAP {} names menu {}, which the mockup registry does not define", p.id(), menu.mock));
        }
    }
    let mut seen = BTreeSet::new();
    for (id, why) in excluded_mock_menus {
        if !seen.insert(*id) {
            problems.push(format!("{id} is listed in EXCLUDED_MOCK_MENUS and again in EXCLUDED_MOCK_MENUS"));
        }
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED_MOCK_MENUS {id} has no reason"));
        }
        if !is_defined(id) {
            problems.push(format!("EXCLUDED_MOCK_MENUS lists {id}, which the mockup registry does not define"));
        }
    }
    if !problems.is_empty() {
        bail!("menu drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Whether `host` is a host whose modals the crawl inventories: a compared host
/// ([`Page::is_compared_host`]), a compared menu target, or an excluded menu of a compared host
/// ([`excluded_menu_id`]).
pub fn is_modal_host(page_map: &[Page], excluded_menus: &[(&str, &str, &str)], host: &str) -> bool {
    let hosts = compared_hosts(page_map);
    let page = hosts.iter().any(|h| h == host);
    let menu = page_map.iter().any(|p| p.is_menu() && p.is_compared_target() && p.id() == host);
    let excluded_menu = excluded_menus.iter().any(|(h, kind, _)| hosts.iter().any(|c| c == h) && excluded_menu_id(h, kind) == host);
    page || menu || excluded_menu
}

/// Checks the modal exclusions of `lists` against its page map and menu exclusions, which its
/// problems name `EXCLUDED_MODALS`, `PAGE_MAP` and `EXCLUDED_MENUS`: each exclusion is listed
/// once, with a reason, names as its host one whose modals the crawl inventories
/// ([`is_modal_host`]), and a
/// `<module>/<action>` of `[a-z0-9._-]+` ids, and is not a modal entry of that host too.
pub fn validate_excluded_modals(lists: &Lists) -> Result<()> {
    let Lists { page_map, excluded_menus, excluded_modals, .. } = *lists;
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (host, action, why) in excluded_modals {
        if !seen.insert((*host, *action)) {
            problems.push(format!("{host} modal {action} is listed in EXCLUDED_MODALS and again in EXCLUDED_MODALS"));
        }
        if why.why.trim().is_empty() {
            problems.push(format!("EXCLUDED_MODALS {host} modal {action} has no reason"));
        }
        if !is_modal_host(page_map, excluded_menus, host) {
            problems.push(format!(
                "EXCLUDED_MODALS lists {host} modal {action}, and {host} is no PAGE_MAP page or tab that is not pending, menu target of one or EXCLUDED_MENUS menu"
            ));
        }
        if !is_action_path(action) {
            problems.push(format!("EXCLUDED_MODALS {host} names action {action:?}, which is not a <module>/<action> of [a-z0-9._-]+ ids"));
        }
        if page_map.iter().any(|p| p.modal.is_some_and(|m| m.action_path() == *action) && p.host().id() == *host) {
            problems.push(format!("{host} modal {action} is listed in PAGE_MAP and again in EXCLUDED_MODALS"));
        }
    }
    if !problems.is_empty() {
        bail!("modal drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Checks that each `EXCLUDED_MOCK_MODALS` claim of `lists` names as its host a compared target
/// of its page map ([`Page::is_compared_target`]), the hosts [`validate_mock_modals`] is handed:
/// a claim on any other host, an `EXCLUDED_MENUS` menu among them, is never checked on either
/// side. The crawl runs it before it fetches anything.
pub fn validate_mock_modal_hosts(lists: &Lists) -> Result<()> {
    let Lists { page_map, excluded_mock_modals, .. } = *lists;
    let problems: Vec<String> = excluded_mock_modals
        .iter()
        .filter(|x| !page_map.iter().any(|p| p.is_compared_target() && p.id() == x.host))
        .map(|MockModalExclusion { host, opener, .. }| {
            format!("EXCLUDED_MOCK_MODALS {host} {opener} names host {host}, which is no PAGE_MAP page or tab that is not pending or menu target of one")
        })
        .collect();
    if !problems.is_empty() {
        bail!("modal drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Checks the mockup's modal openers against the modal entries and exclusions of `lists`,
/// which its problems name `PAGE_MAP`, `EXCLUDED_MOCK_MODALS` and `EXCLUDED_MODALS`. `drawn` is
/// each compared page, tab and menu target's id with the modal openers its mockup render draws
/// ([`crate::mock::probe`]), every such host listed, an empty list when it draws none.
/// The shell palette (`palette_onclick`, which problems name `PALETTE_ONCLICK`) is drawn by some
/// host and listed by no exclusion. Each other opener drawn is a modal entry of that host or an
/// `EXCLUDED_MOCK_MODALS` entry, not both;
/// a modal entry hosted by a listed host names an opener it draws; an exclusion is listed once,
/// with a reason and, when it names a portal action, a `<module>/<action>` of `[a-z0-9._-]+`
/// ids, and names an opener its host draws (else it is stale, a pending or unknown host drawing
/// nothing checked). An `EXCLUDED_MODALS` [`ModalWhy`] claim holds when its mockup host has a
/// compared render and every modal opener that render draws is the palette, a modal entry of
/// another action there or an exclusion there whose portal side is another action; one whose
/// portal side is nothing could be the claimed action's opener, so it does not hold the claim.
pub fn validate_mock_modals(drawn: &[(String, Vec<MockOpener<String>>)], lists: &Lists) -> Result<()> {
    let Lists { page_map, excluded_menus, excluded_modals, excluded_mock_modals, .. } = *lists;
    let drawn_on = |host: &str| drawn.iter().find(|(h, _)| h == host).map(|(_, o)| o.as_slice());
    let mapped = |host: &str, opener: MockOpener<&str>| {
        page_map.iter().find(|p| p.modal.is_some_and(|m| MockOpener::Pages(m.opener) == opener) && p.host().id() == host)
    };
    let excluded = |host: &str, opener: MockOpener<&str>| excluded_mock_modals.iter().find(|x| x.host == host && x.opener == opener);
    let palette = MockOpener::Onclick(lists.palette_onclick);
    let mut problems = Vec::new();
    if !drawn.iter().any(|(_, openers)| openers.iter().any(|o| o.as_str() == palette)) {
        problems.push(format!("PALETTE_ONCLICK declares the shell palette {palette}, which no compared mockup render draws as a modal"));
    }
    for (host, openers) in drawn {
        for opener in openers.iter().filter(|o| o.as_str() != palette) {
            match (mapped(host, opener.as_str()).is_some(), excluded(host, opener.as_str()).is_some()) {
                (false, false) => problems.push(format!("mockup modal {host} {opener} is in neither PAGE_MAP nor EXCLUDED_MOCK_MODALS")),
                (true, true) => problems.push(format!("mockup modal {host} {opener} is listed in PAGE_MAP and again in EXCLUDED_MOCK_MODALS")),
                _ => {}
            }
        }
    }
    for p in page_map {
        let Some(modal) = p.modal else { continue };
        let host = p.host().id();
        if drawn_on(&host).is_some_and(|openers| !openers.iter().any(|o| o.as_str() == MockOpener::Pages(modal.opener))) {
            problems.push(format!("PAGE_MAP {} opens PAGES.{}, which the {host} mockup render does not draw as a modal", p.id(), modal.opener));
        }
    }
    let mut seen = BTreeSet::new();
    for MockModalExclusion { host, opener, portal, why } in excluded_mock_modals {
        if !seen.insert((*host, *opener)) {
            problems.push(format!("{host} {opener} is listed in EXCLUDED_MOCK_MODALS and again in EXCLUDED_MOCK_MODALS"));
        }
        if why.trim().is_empty() {
            problems.push(format!("EXCLUDED_MOCK_MODALS {host} {opener} has no reason"));
        }
        if *opener == palette {
            problems.push(format!("EXCLUDED_MOCK_MODALS lists {host} {opener}, the shell palette PALETTE_ONCLICK declares on every host"));
        }
        if let Some(action) = portal.action() {
            if !is_action_path(action) {
                problems.push(format!("EXCLUDED_MOCK_MODALS {host} {opener} names portal action {action:?}, which is not a <module>/<action> of [a-z0-9._-]+ ids"));
            }
        }
        if !drawn_on(host).is_some_and(|openers| openers.iter().any(|o| o.as_str() == *opener)) {
            problems.push(format!("EXCLUDED_MOCK_MODALS lists {host} {opener}, which no compared {host} mockup render draws as a modal"));
        }
    }
    for (host, action, _) in excluded_modals {
        let mock_host = excluded_menus.iter().find(|(h, kind, _)| excluded_menu_id(h, kind) == *host).map_or(*host, |(h, _, _)| *h);
        let Some(openers) = drawn_on(mock_host) else {
            problems.push(format!("EXCLUDED_MODALS {host} modal {action} says the mockup has no opener of it, but no compared {mock_host} mockup render was checked"));
            continue;
        };
        for opener in openers.iter().map(MockOpener::as_str).filter(|o| *o != palette) {
            let mapped_other = mapped(mock_host, opener).is_some_and(|p| p.modal.is_some_and(|m| m.action_path() != *action));
            let excluded_other = excluded(mock_host, opener).is_some_and(|x| x.portal.action().is_some_and(|other| other != *action));
            if !mapped_other && !excluded_other {
                problems.push(format!(
                    "EXCLUDED_MODALS {host} modal {action} says the mockup has no opener of it, but {mock_host} draws {opener}, which is neither a PAGE_MAP modal entry nor an EXCLUDED_MOCK_MODALS entry of another action there"
                ));
            }
        }
    }
    if !problems.is_empty() {
        bail!("modal drift:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// The page map entry of `lists` whose [`Page::id`] is `page`; an excluded page or tab of
/// `lists` fails with its reason.
pub fn lookup<'m>(lists: &Lists<'m>, page: &str) -> Result<&'m Page> {
    if let Some(p) = lists.page_map.iter().find(|p| p.id() == page) {
        return Ok(p);
    }
    if let Some((_, why)) = lists.excluded.iter().find(|(p, _)| *p == page) {
        bail!("page {page} is excluded: {why}");
    }
    if let Some((_, why)) = lists.excluded_tabs.iter().find(|(t, _)| *t == page) {
        bail!("tab {page} is excluded: {why}");
    }
    bail!("unknown page {page}; pages: {}", lists.page_map.iter().map(Page::id).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod pages_tests;
