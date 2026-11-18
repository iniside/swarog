use super::*;

/// The ids of a serialized `ActionItem` list.
fn action_ids(actions: &serde_json::Value) -> Vec<&str> {
    actions.as_array().expect("an action list").iter().map(|a| a["id"].as_str().expect("an action id")).collect()
}

/// What the WorkQueue table contract and `TableView.page_label` hand the templates. Until
/// Step 11 part b draws these fields the goldens of the same cases cannot see them, so this
/// pins the view model itself; the attempts bar and the row/tool split are drawn already,
/// and are pinned here as well as in the goldens.
#[test]
fn queue_and_table_views_carry_the_work_queue_table_contract() {
    let view = queue_table_contract_case().view();
    let q = &view["body"];
    assert_eq!(q["type"], "Queue");
    assert_eq!(q["item_label"], "Message");
    assert_eq!(
        q["columns"],
        serde_json::json!([
            {"label": "To", "num": false, "sorted": null, "contributed": false},
            {"label": "Account", "num": false, "sorted": null, "contributed": false},
            {"label": "Template", "num": false, "sorted": null, "contributed": false},
            {"label": "Size", "num": true, "sorted": null, "contributed": false},
        ])
    );
    assert_eq!(q["selectable"], true);
    assert_eq!(q["toolbar"], true);
    assert_eq!(q["search_placeholder"], "Recipient, template or message id");
    assert_eq!(q["q"], "anna");
    assert_eq!(q["search_url"], "/admin/_panel/golden/panel?template=welcome");
    assert_eq!(q["filters"], serde_json::json!([{"label": "Template", "value": "welcome"}]));
    assert_eq!(q["foot"], true);
    assert_eq!(q["total"], "3 of 309 messages");
    assert_eq!(q["size"], 25);
    assert_eq!(q["next_url"], "/admin/_panel/golden/panel?q=anna&template=welcome&cursor=c2");
    assert_eq!(q["page_label"], "1 / 13");
    let items = q["items"].as_array().expect("queue items");
    let cells = &items[0]["cells"];
    assert_eq!(cells[0]["cell"]["kind"], "user");
    assert_eq!(cells[0]["cell"]["href"], "/admin/p/mail/outbox?q=anna%40example.com&state=pending");
    assert_eq!(cells[1]["cell"]["kind"], "ref");
    assert_eq!(cells[1]["cell"]["href"], "/admin/e/account/a-17");
    assert_eq!(cells[2]["cell"]["value"], "welcome");
    let nums: Vec<bool> =
        cells.as_array().expect("cells").iter().map(|c| c["num"].as_bool().expect("a num flag")).collect();
    assert_eq!(nums, [false, false, false, true]);
    // Five squares whatever `max_attempts` says (2, 8 and 1 here); the title keeps the count.
    assert_eq!(items[0]["attempts"], serde_json::json!(["f", "", "", "", ""]));
    assert_eq!(items[1]["attempts"], serde_json::json!(["f", "f", "f", "f", "f"]));
    assert_eq!(items[1]["attempts_title"], "7 attempts");
    assert_eq!(items[2]["attempts"], serde_json::json!(["ok", "", "", "", ""]));
    assert_eq!(action_ids(&items[0]["actions"]), ["cancel"]);
    assert_eq!(action_ids(&items[1]["actions"]), ["retry", "cancel"]);
    assert!(action_ids(&items[2]["actions"]).is_empty());
    assert!(action_ids(&view["tools"]).is_empty(), "a QueueRow action is never a panel tool");

    let view = queue_panel_tool_case().view();
    assert_eq!(action_ids(&view["tools"]), ["pause"], "a PanelTool on a WorkQueue draws in the panel tools");
    assert_eq!(view["tools"][0]["queue_row"], false);
    assert_eq!(view["tools"][0]["enabled"], false);
    let item = &view["body"]["items"][0];
    assert_eq!(action_ids(&item["actions"]), ["cancel"], "an item never draws a PanelTool it lists");
    assert_eq!(item["actions"][0]["queue_row"], true);

    let view = queue_later_page_case().view();
    let q = &view["body"];
    assert_eq!(q["page_label"], serde_json::Value::Null, "a cursor does not say how far in it is");
    assert_eq!(q["next_url"], serde_json::Value::Null);
    assert_eq!(q["total"], "2 of 27 jobs");
    assert_eq!(q["foot"], true);
    assert_eq!(q["toolbar"], false);
    assert_eq!(q["selectable"], false);
    assert_eq!(q["item_label"], "Job");
    let view = queue_items_actions_case().view();
    assert_eq!(view["body"]["total"], "2 jobs");
    assert_eq!(view["body"]["page_label"], "1 / 1");
    let empty = PanelCase::new(PanelState::Empty, Some(body(Primitive::WorkQueue(work_queue("pending", Vec::new())))));
    assert_eq!(empty.view()["body"]["foot"], false);

    let page_label = |case: PanelCase| case.view()["body"]["page_label"].clone();
    assert_eq!(page_label(table_first_page_case()), "1 / 50");
    assert_eq!(
        page_label(PanelCase { query: posted(&[("cursor", "c2")]), ..table_first_page_case() }),
        serde_json::Value::Null
    );
    let rows = || vec![player_row("p-1001", "12", "active", false)];
    let table = |total: Option<u64>, next: Option<&str>| {
        PanelCase::new(PanelState::Ok, Some(body(players_table(rows(), total, next))))
    };
    assert_eq!(page_label(table(None, Some("c2"))), serde_json::Value::Null, "an unknown total with more pages");
    assert_eq!(page_label(table(None, None)), "1 / 1");
    assert_eq!(page_label(table(Some(0), None)), "1 / 1");
    assert_eq!(page_label(table(Some(26), Some("c2"))), "1 / 2");
}

/// A queue a search or a filter value narrowed to nothing keeps its toolbar, so the search
/// can be changed back; the same queue unfiltered, its filter at `any`, is plainly empty.
#[test]
fn a_queue_narrowed_to_nothing_keeps_its_toolbar() {
    let env = render::env().expect("templates compile");
    let render = |case: PanelCase| case.render(&env).expect("renders");
    for (what, raw) in [
        ("a search", render(queue_narrowed_case(&[("q", "zed")], Some("zed")))),
        ("a filter value", render(queue_narrowed_case(&[("template", "welcome")], None))),
    ] {
        assert!(raw.contains("class=\"tbl-toolbar\""), "{what}: the toolbar is drawn: {raw}");
        assert!(raw.contains("No matching items") && raw.contains("Nothing matches the search or filters."), "{what}");
        assert!(!raw.contains("Queue is empty"), "{what}");
    }
    let searched = render(queue_narrowed_case(&[("q", "zed")], Some("zed")));
    assert!(searched.contains("value=\"zed\""), "the search box keeps the search");
    let unfiltered = render(queue_narrowed_case(&[("template", "any")], None));
    assert!(!unfiltered.contains("tbl-toolbar"), "an unfiltered empty queue draws no toolbar: {unfiltered}");
    assert!(unfiltered.contains("Queue is empty") && !unfiltered.contains("No matching items"));
}

/// "Load older" from a page already loaded with `before=` (and a stray `cursor=`) carries
/// the timeline's new `before` and the other parameters, never the old paging.
#[test]
fn timeline_older_url_carries_only_the_new_before() {
    let case = PanelCase {
        query: posted(&[("q", "anna"), ("cursor", "c9"), ("before", "2026-10-22T00:00:00Z")]),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: Vec::new(),
                cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                live: false,
                days: false,
                date_only: false,
            }))),
        )
    };
    let older = case.view()["body"]["older_url"].clone();
    let older = older.as_str().expect("an older url");
    assert!(!older.contains("cursor"), "{older}");
    assert_eq!(older.matches("before=").count(), 1, "{older}");
    assert_eq!(older, "/admin/_panel/golden/panel?q=anna&before=2026-10-20T23%3A59%3A00Z");
}

/// The `<th>` texts, then each row's `<td>` inner HTML, of the one table in `raw`.
pub(crate) fn table_grid(raw: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let between = |s: &str, open: &str, close: &str| -> String {
        let at = s.find(open).unwrap_or_else(|| panic!("{open} in {s}")) + open.len();
        s[at..at + s[at..].find(close).unwrap_or_else(|| panic!("{close} in {s}"))].to_string()
    };
    let head = between(raw, "<thead><tr>", "</tr></thead>");
    let heads = head.split("<th").skip(1).map(|th| between(th, ">", "</th>")).collect();
    let rows = between(raw, "<tbody>", "</tbody>")
        .split("</tr>")
        .filter(|r| r.contains("<td"))
        .map(|r| r.split("<td").skip(1).map(|td| between(td, ">", "</td>")).collect())
        .collect();
    (heads, rows)
}

/// The list columns land at `list_columns_at` in contribution order, the ok column holds its
/// cell for the row it answered and an empty cell for every other row, the failed column the
/// alert marker in every row and one warn banner; the foot counts in the item noun.
#[test]
fn table_list_columns_draw_cells_markers_and_one_banner_per_failed_column() {
    let env = render::env().expect("templates compile");
    let raw = table_list_columns_case().render(&env).expect("renders");
    let (heads, rows) = table_grid(&raw);
    assert_eq!(heads, ["Player", "<span>Rank</span>", "<span>Guild</span>", "Level <span>↓</span>", "Status"]);
    assert_eq!(rows.len(), 4);
    assert!(rows[0][1].contains("gold"), "the answered player's cell: {:?}", rows[0]);
    for (i, row) in rows.iter().enumerate().skip(1) {
        assert_eq!(row[1], "", "row {i} was not answered: {row:?}");
    }
    let marker = r#"<span class="muted" title="guilds module unavailable">"#;
    for (i, row) in rows.iter().enumerate() {
        assert!(row[2].starts_with(marker), "row {i} draws the marker: {row:?}");
    }
    assert_eq!(raw.matches(r#"<div class="banner warn">"#).count(), 1, "{raw}");
    assert!(raw.contains("Guild column unavailable — guilds module did not answer. Other columns are live."));
    assert!(raw.contains("1,234 players · showing 1–4"), "the foot counts in the noun: {raw}");
    let user = &rows[0][0];
    assert!(!user.starts_with("<a "), "a row that opens an entity links the name inside: {user}");
    assert!(user.contains(r#"<a href="&#x2f;admin&#x2f;e&#x2f;player&#x2f;p-1001""#), "{user}");
}

/// The labelled draw: `p-1` has a label, `p-2` none.
fn player_labels() -> Labels {
    Labels::from([(("player".to_string(), "p-1".to_string()), "Anna#1".to_string())])
}

fn player_link(id: &str, text: &str) -> Cell {
    Cell { link: Some(Link::Entity(entity("player", id))), ..cell(CellKind::User, text) }
}

/// A table cell drawing its entity as its raw id draws the label where one is held and the
/// id where none is; a linked cell whose text is not the raw id keeps its text.
#[test]
fn a_table_cell_draws_its_entitys_label_or_its_plain_id() {
    let view = table_labelled_case().view();
    let value = |row: usize| view["body"]["rows"][row]["cells"][0]["cell"]["value"].clone();
    assert_eq!(value(0), "Anna#1001");
    assert_eq!(value(1), "p-1002");

    let row = |c: Cell| Row { entity: None, cells: vec![c], highlight: false, item: None, actions: Vec::new() };
    let rows = vec![row(player_link("p-1", "p-1")), row(player_link("p-2", "p-2")), row(player_link("p-1", "Anna K"))];
    let case = PanelCase {
        labels: player_labels(),
        ..PanelCase::new(PanelState::Ok, Some(body(players_table(rows, None, None))))
    };
    let view = case.view();
    let drawn: Vec<&serde_json::Value> =
        (0..3).map(|r| &view["body"]["rows"][r]["cells"][0]["cell"]["value"]).collect();
    assert_eq!(drawn, ["Anna#1", "p-2", "Anna K"]);
}

/// A timeline actor and target and a queue item's entity draw the label where one is held
/// and the plain id where none is.
#[test]
fn a_timeline_actor_and_target_and_a_queue_entity_draw_the_label_or_the_plain_id() {
    let entry = |id: &str| TimelineEntry {
        at: "2026-11-19T10:00:00Z".to_string(),
        actor: Some(player_link(id, id)),
        title: None,
        verb: "gifted".to_string(),
        target: Some(entity("player", id)),
        summary: String::new(),
        badges: Vec::new(),
        detail: None,
        icon: String::new(),
        tone: None,
    };
    let timeline =
        Timeline { entries: vec![entry("p-1"), entry("p-2")], cursor_before: None, live: false, days: false, date_only: false };
    let labelled = |p: Primitive| PanelCase { labels: player_labels(), ..PanelCase::new(PanelState::Ok, Some(body(p))) };
    let view = labelled(Primitive::Timeline(timeline)).view();
    let item = |i: usize, part: &str| view["body"]["days"][0]["items"][i][part]["value"].clone();
    assert_eq!([item(0, "actor"), item(0, "target")], ["Anna#1", "Anna#1"]);
    assert_eq!([item(1, "actor"), item(1, "target")], ["p-2", "player:p-2"]);

    let queued = |id: &str| QueueItem { entity: Some(entity("player", id)), ..queue_item(id, "failed", 1, 5, &[]) };
    let queue = Primitive::WorkQueue(work_queue("failed", vec![queued("p-1"), queued("p-2")]));
    let both = BTreeMap::from([("player".to_string(), vec!["p-1".to_string(), "p-2".to_string()])]);
    assert_eq!(crate::engine::labels::refs(&queue), both, "each queue item's entity is drawn");
    let view = labelled(queue).view();
    let value = |i: usize| view["body"]["items"][i]["entity"]["value"].clone();
    assert_eq!([value(0), value(1)], ["Anna#1", "p-2"]);
}

/// An action's id and the `(entity, item)` it is bound to.
type Binding = (String, Option<String>, Option<String>);

/// The binding of each row's actions in `view`.
fn row_bindings(view: &serde_json::Value) -> Vec<Vec<Binding>> {
    view["body"]["rows"]
        .as_array()
        .expect("table rows")
        .iter()
        .map(|r| {
            r["actions"]
                .as_array()
                .expect("row actions")
                .iter()
                .map(|a| {
                    let vals: BTreeMap<String, String> =
                        serde_json::from_str(a["vals"].as_str().expect("vals")).expect("vals is JSON");
                    let id = a["id"].as_str().expect("an id").to_string();
                    (id, vals.get(actions::FIELD_ENTITY).cloned(), vals.get(actions::FIELD_ITEM).cloned())
                })
                .collect()
        })
        .collect()
}

/// On an entity tab, a row with no entity binds its actions to the panel's entity and a row
/// with its own entity keeps it; on a list page (no panel entity) the first row binds none.
/// `TableRow` actions draw on their rows only, never as tools.
#[test]
fn table_row_actions_bind_the_row_entity_else_the_panel_entity() {
    let some = |s: &str| Some(s.to_string());
    let on_tab = table_row_actions_case().view();
    let rows = row_bindings(&on_tab);
    let first: Vec<_> = ["inspect", "revoke", "open", "wipe"]
        .iter()
        .map(|id| (id.to_string(), some("player:p-1001"), some("sess-1")))
        .collect();
    assert_eq!(rows[0], first, "an entity-less row binds the panel's entity");
    assert_eq!(rows[1], [("inspect".to_string(), some("device:d-9"), some("sess-2"))], "a row's own entity wins");
    assert!(rows[2].is_empty());
    let url = on_tab["body"]["rows"][0]["actions"][0]["url"].as_str().expect("a url");
    assert_eq!(url, "/admin/_action/golden/inspect?entity=player%3Ap-1001&item=sess-1");
    assert_eq!(action_ids(&on_tab["tools"]), ["export"], "a TableRow action is never a panel tool");
    assert_eq!(on_tab["body"]["row_actions"], true);

    let on_list = PanelCase { entity: None, ..table_row_actions_case() }.view();
    let rows = row_bindings(&on_list);
    assert_eq!(rows[0][0], ("inspect".to_string(), None, some("sess-1")), "no panel entity to fall back to");
    assert_eq!(rows[1][0], ("inspect".to_string(), some("device:d-9"), some("sess-2")));
}

/// The header fragment's out-of-band `#crumb-cur` shows the header's crumb, else its title.
#[test]
fn header_crumb_replaces_the_last_breadcrumb_else_the_title_does() {
    let env = render::env().expect("templates compile");
    let render = |crumb: Option<&str>| {
        let header = EntityHeader {
            title: "Anna Kowalska".to_string(),
            crumb: crumb.map(str::to_string),
            subtitle: String::new(),
            status: None,
            badges: Vec::new(),
            avatar_seed: "p-1001".to_string(),
        };
        let case = PanelCase {
            mode: "header",
            ..PanelCase::new(PanelState::Ok, Some(body(Primitive::EntityHeader(header))))
        };
        case.render(&env).expect("renders")
    };
    let oob = |text: &str| format!(r#"<span class="cur" id="crumb-cur" hx-swap-oob="true">{text}</span>"#);
    assert!(render(Some("anna.k")).contains(&oob("anna.k")), "{}", render(Some("anna.k")));
    assert!(render(None).contains(&oob("Anna Kowalska")), "{}", render(None));
}
