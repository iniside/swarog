use super::*;

// ---- cases ---------------------------------------------------------------------

/// Every case as `(name, golden form)`, in a fixed order.
pub(super) fn cases(env: &Environment<'_>) -> Vec<Case> {
    let mut out: Vec<Rendered> = Vec::new();
    // Each case is collected around building and rendering it alone, so a view built and not
    // pushed counts for nothing.
    macro_rules! push {
        ($rendered:expr) => {
            out.push(scoped(|| $rendered))
        };
    }
    macro_rules! panel {
        ($name:expr, $case:expr $(,)?) => {
            push!({
                let case: PanelCase = $case;
                Rendered {
                    name: $name,
                    result: case.render(env),
                    primitive: case.body.as_ref().map(|b| primitive_case(&b.primitive)),
                    state: (case.mode == "panel").then(|| state_case(case.state)),
                    seen: render::seen::Record::new(),
                    refs: case.body.as_ref().map(|b| collected(&b.primitive)).unwrap_or_default(),
                    asked: render::asked::Asked::new(),
                    refused: render::refused::Refused::new(),
                }
            })
        };
    }

    // ---- each PanelState, in panel chrome ----
    panel!("state_ok", PanelCase::new(PanelState::Ok, Some(body(summary()))));
    panel!("state_empty", PanelCase::new(PanelState::Empty, None));
    panel!(
        "state_empty_payload",
        PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::Empty {
                title: "No grants yet".to_string(),
                detail: "Grants appear here once issued.".to_string(),
            })),
        ),
    );
    let readonly_tool = decl("restart", "Restart", "refresh", Then::Invoke);
    panel!(
        "state_readonly",
        PanelCase {
            actions: vec![action(MODULE, &readonly_tool, Role::Admin, false)],
            ..PanelCase::new(
                PanelState::Readonly,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Info, text: "Managed by the deploy pipeline.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    let declared_text = "Managed by the deploy pipeline: edit it in the infra repo.";
    let declared_manifest = Manifest {
        module: MODULE.to_string(),
        actions: vec![readonly_tool.clone()],
        contributions: vec![ContributionDecl {
            readonly: Some(declared_text.to_string()),
            ..contribution(CONTRIBUTION, Point::Page("overview".to_string()), Role::Viewer)
        }],
        writes: true,
        ..Manifest::default()
    };
    panel!(
        "state_readonly_declared",
        PanelCase {
            readonly: declared_manifest.contributions[0].readonly.clone(),
            actions: routes::panel_tools(&operator(Role::Admin), CSRF, MODULE, &declared_manifest, CONTRIBUTION),
            ..PanelCase::new(PanelState::Readonly, Some(body(summary())))
        },
    );
    panel!(
        "state_stale_hides_module_banner",
        PanelCase {
            error: Some("deadline of 2000ms elapsed"),
            age_s: Some(185),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(
                PanelState::Stale,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Warn, text: "This banner shows only when fresh.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    panel!(
        "state_error",
        PanelCase {
            error: Some("connection refused"),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(PanelState::Error, None)
        },
    );
    panel!("state_error_no_detail", PanelCase::new(PanelState::Error, None));
    panel!("state_denied", PanelCase { min_role: "admin", ..PanelCase::new(PanelState::Denied, None) });

    // ---- KPI tiles: delta tones, then each state ----
    let tile = |d: Option<Delta>| PanelCase::tile(PanelState::Ok, Some(body(Primitive::KpiTile(kpi(d)))));
    panel!("tile_delta_ok_up", tile(delta("+4.2%", Some(Tone::Ok))));
    panel!(
        "tile_delta_err_down",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                unit: Some("ms".to_string()),
                spark: Palette::Chart2,
                link: Some(page_link("platform", "latency")),
                ..kpi(delta("+18 ms", Some(Tone::Err)))
            }))),
        ),
    );
    panel!(
        "tile_delta_warn_bad_up_capacity",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                spark: Palette::Warn,
                capacity: Some((62.0, 100.0)),
                ..kpi(delta("+9", Some(Tone::Warn)))
            }))),
        ),
    );
    panel!(
        "kpi_tiles",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTiles(vec![
                kpi(delta("+4.2%", Some(Tone::Ok))),
                KpiTile { label: "Matches running".to_string(), value: "318".to_string(), ..kpi(None) },
            ]))),
        ),
    );
    panel!(
        "kpi_tiles_panel_point",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::KpiTiles(vec![
                kpi(delta("+4.2%", Some(Tone::Ok))),
                KpiTile { label: "Matches running".to_string(), value: "318".to_string(), ..kpi(None) },
            ]))),
        ),
    );
    panel!(
        "kpi_panel_point",
        PanelCase::new(PanelState::Ok, Some(body(Primitive::KpiTile(kpi(delta("+4.2%", Some(Tone::Ok))))))),
    );
    panel!("tile_delta_info_flat", tile(delta("±0", Some(Tone::Info))));
    panel!("tile_delta_accent_flat", tile(delta("±0", Some(Tone::Accent))));
    panel!(
        "tile_delta_none_period_only",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                delta: Some(Delta { value: String::new(), period: "last 24 h".to_string(), tone: None }),
                series: vec![4.0],
                ..kpi(None)
            }))),
        ),
    );
    panel!(
        "tile_stale",
        PanelCase {
            age_s: Some(185),
            error: Some("deadline of 2000ms elapsed"),
            ..PanelCase::tile(PanelState::Stale, Some(body(Primitive::KpiTile(kpi(delta("+4.2%", Some(Tone::Ok)))))))
        },
    );
    panel!(
        "tile_error_last_value",
        PanelCase {
            error: Some("connection refused"),
            last_value: Some(LastValue { value: "12,408 players".to_string(), age_s: 420 }),
            ..PanelCase::tile(PanelState::Error, None)
        },
    );
    panel!("tile_error", PanelCase { error: Some("connection refused"), ..PanelCase::tile(PanelState::Error, None) });
    panel!("tile_empty", PanelCase::tile(PanelState::Empty, None));
    panel!("tile_denied", PanelCase { min_role: "operator", ..PanelCase::tile(PanelState::Denied, None) });

    // ---- panel chrome: heading, meta, note, module banner in every tone ----
    panel!(
        "chrome_heading_meta_note",
        PanelCase::new(
            PanelState::Ok,
            Some(PanelBody {
                heading: Some("eu-west-1".to_string()),
                meta: Some("3 nodes".to_string()),
                note: Some("Sampled every 30 s".to_string()),
                ..body(summary())
            }),
        ),
    );
    for (name, tone) in [
        ("chrome_banner_ok", Tone::Ok),
        ("chrome_banner_warn", Tone::Warn),
        ("chrome_banner_err", Tone::Err),
        ("chrome_banner_info", Tone::Info),
    ] {
        panel!(
            name,
            PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    banner: Some(Banner { tone, text: "Module banner text.".to_string() }),
                    ..body(summary())
                }),
            ),
        );
    }

    // ---- tools: emphasis, trailing icon, danger, disabled, when_empty ----
    let primary = ActionDecl { emphasis: Emphasis::Primary, ..decl("deploy", "Deploy", "up", Then::Invoke) };
    let default = decl("edit", "Edit", "edit", Then::Form(profile_form()));
    let ghost = ActionDecl {
        emphasis: Emphasis::Ghost,
        ..decl("pause", "Pause", "pause", Then::Confirm(confirm("Pause?", ConfirmTone::Warn)))
    };
    let trailing = ActionDecl {
        emphasis: Emphasis::Ghost,
        trailing_icon: true,
        ..decl("logs", "Open logs", "external", Then::Link(page_link("platform", "logs")))
    };
    let danger = ActionDecl {
        danger: true,
        emphasis: Emphasis::Primary,
        ..decl("drain", "Drain", "trash", Then::Confirm(confirm("Drain?", ConfirmTone::Danger)))
    };
    let denied = ActionDecl { min_role: Role::Admin, ..decl("wipe", "Wipe", "trash", Then::Invoke) };
    let unimplemented = ActionDecl { implemented: false, ..decl("export", "Export", "download", Then::Invoke) };
    panel!(
        "tools_emphasis",
        PanelCase {
            entity: Some(entity("server", "eu-1")),
            actions: vec![
                tool(&primary),
                tool(&default),
                tool(&ghost),
                tool(&trailing),
                tool(&danger),
                action(MODULE, &denied, Role::Operator, true),
                tool(&unimplemented),
            ],
            ..PanelCase::new(PanelState::Ok, Some(body(summary())))
        },
    );
    let create = ActionDecl {
        when_empty: true,
        emphasis: Emphasis::Primary,
        ..decl("create", "Create key", "plus", Then::Invoke)
    };
    panel!(
        "tools_when_empty",
        PanelCase { actions: vec![tool(&create), tool(&default)], ..PanelCase::new(PanelState::Empty, None) },
    );
    panel!(
        "tools_when_empty_payload",
        PanelCase {
            actions: vec![tool(&create), tool(&default)],
            ..PanelCase::new(
                PanelState::Empty,
                Some(body(Primitive::Empty {
                    title: "No keys".to_string(),
                    detail: "Create the first one.".to_string(),
                })),
            )
        },
    );

    // ---- PropertyList and cells ----
    let rotate = ActionDecl {
        danger: true,
        ..decl("rotate", "Rotate", "refresh", Then::Confirm(confirm("Rotate key?", ConfirmTone::Danger)))
    };
    let copy = decl("copy", "Copy", "copy", Then::Invoke);
    panel!(
        "props_code_footer_tools",
        PanelCase {
            entity: Some(entity("apikey", "k-7")),
            actions: vec![tool(&rotate), tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Rotated 3 days ago".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        code: Some("curl -H 'X-Api-Key: gb_live_…' https://api.example/v1/ping".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel!(
        "props_item_head_tools",
        PanelCase {
            actions: vec![tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Note in the head".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel!(
        "props_two_columns",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(PropertyList {
                columns: 2,
                ..props(vec![
                    ("Version", cell(CellKind::Mono, "1.4.2")),
                    ("Region", cell(CellKind::Text, "eu-west")),
                    ("Nodes", cell(CellKind::Number, "3")),
                    ("Disk", cell(CellKind::Bytes, "1.2 GB")),
                ])
            }))),
        ),
    );
    panel!(
        "cell_kinds",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Text", cell(CellKind::Text, "plain")),
                ("Text err", toned(CellKind::Text, "failing", Tone::Err)),
                ("Mono", cell(CellKind::Mono, "a1b2c3")),
                ("Mono warn", toned(CellKind::Mono, "slow", Tone::Warn)),
                ("Badge derived", cell(CellKind::Badge, "failed")),
                ("Badge unknown", cell(CellKind::Badge, "mystery")),
                ("Badge toned", toned(CellKind::Badge, "pending", Tone::Ok)),
                (
                    "Ref",
                    Cell { link: Some(Link::Entity(entity("item", "sword-7"))), ..cell(CellKind::Ref, "item:sword-7") },
                ),
                ("Number", cell(CellKind::Number, "1,204")),
                ("Bytes", cell(CellKind::Bytes, "512 KB")),
                ("Time", cell(CellKind::Time, "2026-10-21 09:12")),
                ("Time info", toned(CellKind::Time, "in 5 min", Tone::Info)),
                (
                    "User",
                    Cell {
                        link: Some(Link::Entity(entity("player", "p-1001"))),
                        ..cell(CellKind::User, "anna.kowalska")
                    },
                ),
            ])))),
        ),
    );
    panel!(
        "cell_bar",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Default", bar("40%", 40, None)),
                ("Ok tone", bar("30%", 30, Some(Tone::Ok))),
                ("Warn", bar("75%", 75, Some(Tone::Warn))),
                ("Err", bar("100%", 100, Some(Tone::Err))),
                ("Clamped", bar("140%", 140, Some(Tone::Info))),
                ("Accent", bar("55%", 55, Some(Tone::Accent))),
            ])))),
        ),
    );

    // ---- ServiceStatus: one and two columns, with and without value_label ----
    panel!("services_one_column", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, None)))));
    panel!("services_one_column_value_label", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, Some("p95"))))));
    panel!("services_two_columns", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, None)))));
    panel!("services_two_columns_value_label", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, Some("lag"))))));

    // ---- DataTable ----
    panel!(
        "table_highlight_row_menu",
        PanelCase {
            query: posted(&[("status", "active"), ("cursor", "c1")]),
            row_menu_kinds: strings(&["player"]),
            ..PanelCase::new(
                PanelState::Ok,
                Some(body(players_table(
                    vec![
                        player_row("p-1001", "12", "active", false),
                        player_row("p-1002", "40", "banned", true),
                        Row {
                            entity: None,
                            cells: vec![
                                cell(CellKind::Text, "system"),
                                cell(CellKind::Number, "0"),
                                cell(CellKind::Badge, "ok"),
                            ],
                            highlight: false,
                            item: None,
                            actions: Vec::new(),
                        },
                    ],
                    Some(1234),
                    Some("c2"),
                ))),
            )
        },
    );
    panel!(
        "table_empty_search",
        PanelCase {
            query: posted(&[("q", "zed")]),
            search: Some("zed"),
            ..PanelCase::new(PanelState::Empty, Some(body(players_table(Vec::new(), Some(0), None))))
        },
    );
    panel!("table_first_page_label", table_first_page_case());
    panel!("table_list_columns", table_list_columns_case());
    panel!("table_labelled", table_labelled_case());
    panel!("table_filter_kinds", table_filter_kinds_case());
    let owners = props(vec![("Owner", player_cell("p-1001")), ("Buyer", player_cell("p-1002"))]);
    panel!("labelled_props", labelled_case("panel", Primitive::PropertyList(owners)));
    let gifted = TimelineEntry {
        at: "2026-10-21T09:12:00Z".to_string(),
        actor: None,
        title: None,
        verb: "gifted".to_string(),
        target: None,
        summary: "a sword".to_string(),
        badges: vec![player_cell("p-1001"), player_cell("p-1002")],
        detail: None,
        icon: String::new(),
        tone: None,
    };
    let badges = Timeline { entries: vec![gifted], cursor_before: None, live: false, days: false, date_only: false };
    panel!("labelled_timeline_badges", labelled_case("panel", Primitive::Timeline(badges)));
    let owned = EntityHeader {
        title: "Sword".to_string(),
        crumb: None,
        subtitle: "item".to_string(),
        status: Some(player_cell("p-1001")),
        badges: vec![player_cell("p-1002")],
        avatar_seed: "sword-7".to_string(),
    };
    panel!("labelled_header", labelled_case("header", Primitive::EntityHeader(owned)));
    let owner = Column {
        key: "owner".to_string(),
        label: "Owner".to_string(),
        kind: CellKind::User,
        sortable: false,
        align: Align::Left,
        sorted: None,
        wrap: false,
    };
    let owned_by = |id: &str, owner: &str| QueueItem { cells: vec![player_cell(owner)], ..queue_item(id, "failed", 1, 5, &[]) };
    let queue = WorkQueue {
        columns: vec![owner],
        ..work_queue("failed", vec![owned_by("job-1", "p-1001"), owned_by("job-2", "p-1002")])
    };
    panel!("labelled_queue_cells", labelled_case("panel", Primitive::WorkQueue(queue)));
    panel!("table_row_actions", table_row_actions_case());
    panel!("table_row_icon_only", table_row_icon_only_case());
    panel!(
        "table_noun_later_page",
        PanelCase {
            query: posted(&[("cursor", "c2")]),
            ..PanelCase::new(PanelState::Ok, Some(body(noun_table(Some(1234)))))
        },
    );
    panel!("table_noun_no_total", PanelCase::new(PanelState::Ok, Some(body(noun_table(None)))));
    panel!("table_sorted_ascending", PanelCase::new(PanelState::Ok, Some(body(sorted_ascending_table()))));
    let one_player = || vec![player_row("p-1001", "12", "active", false)];
    panel!("table_noun_one", PanelCase::new(PanelState::Ok, Some(body(noun_table_of(one_player(), Some(1), None)))));
    panel!("table_wrap_columns", PanelCase::new(PanelState::Ok, Some(body(wrap_table(one_player())))));
    panel!("panel_untitled", PanelCase { title: "", ..PanelCase::new(PanelState::Ok, Some(body(summary()))) });
    panel!(
        "panel_foot_tag",
        PanelCase::new(
            PanelState::Ok,
            Some(PanelBody {
                foot: Some("Synced from the deploy pipeline".to_string()),
                tag: Some("beta".to_string()),
                ..body(summary())
            }),
        ),
    );

    // ---- Timeline ----
    let actor = |kind: CellKind, entity_kind: &str, id: &str| {
        let value = if kind == CellKind::Ref { format!("{entity_kind}:{id}") } else { id.to_string() };
        Cell { link: Some(Link::Entity(entity(entity_kind, id))), ..cell(kind, &value) }
    };
    let entry = |at: &str, actor: Option<Cell>, verb: &str, summary: &str| TimelineEntry {
        at: at.to_string(),
        actor,
        title: None,
        verb: verb.to_string(),
        target: None,
        summary: summary.to_string(),
        badges: Vec::new(),
        detail: None,
        icon: String::new(),
        tone: None,
    };
    let (user, svc) = (Some(actor(CellKind::User, "player", "p-1001")), Some(actor(CellKind::Ref, "svc", "billing")));
    panel!(
        "timeline_two_days_older",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: vec![
                    TimelineEntry {
                        target: Some(entity("item", "sword-7")),
                        detail: Some("batch 42".to_string()),
                        icon: "gift".to_string(),
                        tone: Some(Tone::Ok),
                        ..entry("2026-10-21T09:12:00Z", user, "grant", "granted 1 item")
                    },
                    entry("not-a-time", Some(actor(CellKind::User, "user", "ops.anna")), "", "imported legacy entry"),
                    entry("2026-10-21T08:03:00Z", None, "", "nightly compaction finished"),
                    TimelineEntry {
                        detail: Some("attempt 2 of 5".to_string()),
                        icon: "refresh".to_string(),
                        tone: Some(Tone::Warn),
                        ..entry("2026-10-20T23:59:00Z", svc, "retry", "webhook retried")
                    },
                ],
                cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                live: false,
                days: true,
                date_only: false,
            }))),
        ),
    );
    let rollout = || {
        vec![
            TimelineEntry {
                title: Some("Season 4 rollout".to_string()),
                badges: vec![toned(CellKind::Label, "canary", Tone::Warn), cell(CellKind::Label, "eu-west")],
                ..entry("2026-10-21T09:12:00Z", Some(cell(CellKind::Strong, "deploy-bot")), "", "promoted build 1.4.2")
            },
            entry("2026-10-20T23:59:00Z", Some(actor(CellKind::Ref, "svc", "billing")), "retry", "webhook retried"),
        ]
    };
    panel!(
        "timeline_one_run",
        PanelCase::new(
            PanelState::Ok,
            Some(PanelBody {
                foot: Some("2 events".to_string()),
                ..body(Primitive::Timeline(Timeline {
                    entries: rollout(),
                    cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                    live: false,
                    days: false,
                    date_only: false,
                }))
            }),
        ),
    );
    panel!(
        "timeline_date_only",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: rollout(),
                cursor_before: None,
                live: false,
                days: false,
                date_only: true,
            }))),
        ),
    );
    let no_entries = Timeline { entries: Vec::new(), cursor_before: None, live: false, days: false, date_only: false };
    panel!("timeline_empty", PanelCase::new(PanelState::Empty, Some(body(Primitive::Timeline(no_entries)))));

    // ---- WorkQueue: row actions, a panel tool, the table contract and its foot ----
    panel!("queue_items_actions", queue_items_actions_case());
    panel!(
        "queue_empty",
        PanelCase::new(PanelState::Empty, Some(body(Primitive::WorkQueue(work_queue("pending", Vec::new()))))),
    );
    panel!("queue_panel_tool", queue_panel_tool_case());
    panel!("queue_table_contract", queue_table_contract_case());
    panel!("queue_wrap_column", queue_wrap_column_case());
    panel!("queue_later_page", queue_later_page_case());
    panel!("queue_search_empty", queue_narrowed_case(&[("q", "zed")], Some("zed")));
    panel!("queue_filter_empty", queue_narrowed_case(&[("template", "welcome")], None));
    panel!("queue_unfiltered_empty", queue_narrowed_case(&[], None));

    // ---- TimeSeriesChart ----
    panel!(
        "chart_two_axes",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::TimeSeriesChart(TimeSeriesChart {
                buckets: strings(&["00:00", "06:00", "12:00", "18:00"]),
                series: vec![
                    Series {
                        label: "Requests".to_string(),
                        unit: "rps".to_string(),
                        values: vec![120.0, 340.0, 280.0, 410.0],
                        axis: Axis::Left,
                    },
                    Series {
                        label: "p95".to_string(),
                        unit: "ms".to_string(),
                        values: vec![35.5, 42.0, 38.0, 51.25],
                        axis: Axis::Right,
                    },
                ],
            }))),
        ),
    );

    // ---- the other draw modes: attention, header, fact, count, nav ----
    let attention = Primitive::AttentionItems(vec![
        AttentionItem {
            severity: Severity::Err,
            title: "billing-svc is down".to_string(),
            text: "No heartbeat for 4 min".to_string(),
            link: page_link("platform", "services"),
            action_label: "Open".to_string(),
            since: "4 min".to_string(),
        },
        AttentionItem {
            severity: Severity::Warn,
            title: "3 failed jobs".to_string(),
            text: "Webhook deliveries are retrying".to_string(),
            link: Link::Entity(entity("queue", "webhooks")),
            action_label: "Review".to_string(),
            since: "12 min".to_string(),
        },
    ]);
    let in_mode = |mode: &'static str, state: PanelState, primitive: Option<Primitive>| PanelCase {
        mode,
        error: (state == PanelState::Error).then_some("connection refused"),
        ..PanelCase::new(state, primitive.map(body))
    };
    panel!("attention_ok", PanelCase { icon: "", ..in_mode("attention", PanelState::Ok, Some(attention)) });
    panel!("attention_error", in_mode("attention", PanelState::Error, None));
    let header = EntityHeader {
        title: "Anna Kowalska".to_string(),
        crumb: None,
        subtitle: "player since 2024".to_string(),
        status: Some(cell(CellKind::Badge, "active")),
        badges: Vec::new(),
        avatar_seed: "p-1001".to_string(),
    };
    panel!("header_ok", in_mode("header", PanelState::Ok, Some(Primitive::EntityHeader(header.clone()))));
    let crumbed = EntityHeader {
        crumb: Some("anna.k".to_string()),
        badges: vec![cell(CellKind::Label, "verified"), toned(CellKind::Label, "2FA off", Tone::Warn)],
        ..header
    };
    panel!("header_crumb_badges", in_mode("header", PanelState::Ok, Some(Primitive::EntityHeader(crumbed))));
    panel!("header_error", PanelCase { fallback_title: "p-1001", ..in_mode("header", PanelState::Error, None) });
    let one_cell = |c: Cell| Some(Primitive::Cell(c));
    panel!("fact_cell", in_mode("fact", PanelState::Ok, one_cell(toned(CellKind::Badge, "verified", Tone::Ok))));
    panel!("fact_error", in_mode("fact", PanelState::Error, None));
    panel!("count_cell", in_mode("count", PanelState::Ok, one_cell(cell(CellKind::Number, "14"))));
    panel!("count_error", in_mode("count", PanelState::Error, None));
    panel!("nav_counter_err", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "3", Tone::Err))));
    panel!("nav_counter_warn", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "7", Tone::Warn))));
    panel!("fact_mono_muted", in_mode("fact", PanelState::Ok, one_cell(cell(CellKind::MonoMuted, "sha-9f3c"))));
    panel!("fact_toggle_on", in_mode("fact", PanelState::Ok, one_cell(cell(CellKind::Toggle, "true"))));
    panel!("fact_toggle_off", in_mode("fact", PanelState::Ok, one_cell(cell(CellKind::Toggle, "false"))));
    panel!("fact_dim", in_mode("fact", PanelState::Ok, one_cell(cell(CellKind::Dim, "manual grant"))));
    panel!("fact_strong", in_mode("fact", PanelState::Ok, one_cell(cell(CellKind::Strong, "Anna Kowalska"))));
    panel!("fact_tone_accent", in_mode("fact", PanelState::Ok, one_cell(toned(CellKind::Label, "beta", Tone::Accent))));
    let linked_mono = Cell { link: Some(Link::Entity(entity("build", "1.4.2"))), ..cell(CellKind::Mono, "1.4.2") };
    panel!("fact_linked_mono", in_mode("fact", PanelState::Ok, one_cell(linked_mono)));
    // Another kind's label on an unlinked cell keeps the cell's tag (pages-core.js:132); one
    // that does not resolve draws the raw id.
    let labelled_mono = || Cell { label: Some(entity("character", "c-7")), ..cell(CellKind::Mono, "c-7") };
    let yuki = Labels::from([(("character".to_string(), "c-7".to_string()), "Yuki".to_string())]);
    panel!(
        "fact_labelled_mono",
        PanelCase { labels: yuki, ..in_mode("fact", PanelState::Ok, one_cell(labelled_mono())) },
    );
    panel!("fact_labelled_mono_unresolved", in_mode("fact", PanelState::Ok, one_cell(labelled_mono())));
    let row_cell = Cell {
        row: true,
        more: vec![cell(CellKind::Muted, "since 2024")],
        ..toned(CellKind::Badge, "active", Tone::Ok)
    };
    panel!("fact_row_cell", in_mode("fact", PanelState::Ok, one_cell(row_cell)));

    // ---- panel form: fresh, readonly, readonly beside an enabled tool ----
    panel!(
        "panel_form",
        PanelCase { form: true, ..PanelCase::new(PanelState::Ok, Some(body(Primitive::Form(profile_form())))) },
    );
    panel!("panel_form_readonly", PanelCase::new(PanelState::Readonly, Some(body(Primitive::Form(profile_form())))));
    // The seg above the form, the first mode on and its submit label (pages-platform.js:172).
    // An optional change note with the default label, after the fields and before the actions.
    panel!(
        "panel_form_reason",
        PanelCase {
            form: true,
            ..PanelCase::new(
                PanelState::Ok,
                Some(body(Primitive::Form(FormSpec {
                    reason: ReasonReq::Optional,
                    reason_hint: Some("Why? (recorded in the audit log)".to_string()),
                    ..moded_form()
                }))),
            )
        },
    );
    panel!(
        "panel_form_modes",
        PanelCase { form: true, ..PanelCase::new(PanelState::Ok, Some(body(Primitive::Form(moded_form())))) },
    );
    // The module writes, but the viewer may not use the PanelForm action (so the panel is
    // readonly) while a PanelTool of the same panel stays enabled for them.
    let form_manifest = Manifest {
        module: MODULE.to_string(),
        actions: vec![
            ActionDecl {
                point: ActionPoint::PanelForm(CONTRIBUTION.to_string()),
                min_role: Role::Admin,
                ..decl("save", "Save", "check", Then::Invoke)
            },
            decl("refresh", "Refresh", "refresh", Then::Invoke),
            ActionDecl { min_role: Role::Admin, order: 1, ..decl("purge", "Purge", "trash", Then::Invoke) },
        ],
        writes: true,
        ..Manifest::default()
    };
    panel!(
        "panel_form_readonly_enabled_tool",
        PanelCase {
            actions: routes::panel_tools(&operator(Role::Viewer), CSRF, MODULE, &form_manifest, CONTRIBUTION),
            ..PanelCase::new(PanelState::Readonly, Some(body(Primitive::Form(profile_form()))))
        },
    );

    // ---- the empty cells of the draw-mode x state matrix ----
    panel!("fact_denied", PanelCase { min_role: "admin", ..in_mode("fact", PanelState::Denied, None) });
    panel!(
        "state_stale_no_age",
        PanelCase {
            error: Some("deadline of 2000ms elapsed"),
            ..PanelCase::new(PanelState::Stale, Some(body(summary())))
        },
    );
    // `panels::resolve` errors a ColumnCells answer, so a panel never draws one: its body
    // draws nothing of the cells.
    panel!(
        "column_cells",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::ColumnCells(BTreeMap::from([
                ("Guild".to_string(), cell(CellKind::Text, "Night Owls")),
                ("Rank".to_string(), toned(CellKind::Badge, "officer", Tone::Info)),
            ])))),
        ),
    );

    // ---- a panel form shown again after a 422 and a 409: `routes::render_instance` ----
    let form_decl = ContributionDecl {
        id: CONTRIBUTION.to_string(),
        point: Point::Page("profile".to_string()),
        order: 0,
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 0,
        readonly: None,
    };
    let chrome = Chrome {
        meta: Some("rev 3".to_string()),
        heading: Some("anna".to_string()),
        note: Some("Saved by ops".to_string()),
        banner: Some(Banner { tone: Tone::Info, text: "Edits sync within a minute.".to_string() }),
        foot: None,
        tag: None,
    };
    let player = entity("player", "p-1001");
    let frame_path = format!("/admin/_panel/{MODULE}/{CONTRIBUTION}");
    let frame_query = vec![("entity".to_string(), render::entity_param(&player))];
    let frame_tools = vec![tool(&decl("copy", "Copy", "copy", Then::Invoke))];
    let frame = FormFrame {
        module: MODULE,
        decl: &form_decl,
        mode: "panel",
        path: &frame_path,
        query: &frame_query,
        entity: Some(&player),
        actions: &frame_tools,
        csrf: CSRF,
    };
    let profile_spec = profile_form();
    let profile_body = posted(&[
        ("display_name", ""),
        ("bio", "hi"),
        ("level", "abc"),
        ("ratio", "0.5"),
        ("muted", "false"),
        ("tier", "pro"),
        ("regions", "us"),
        ("meta", r#"{"v":2}"#),
        ("owner", "p-2"),
        ("credit", "5"),
        ("credit__currency", "USD"),
        ("cooldown", "1h"),
    ]);
    let field_errors = BTreeMap::from([
        ("display_name".to_string(), "must not be empty".to_string()),
        ("level".to_string(), "must be an integer".to_string()),
    ]);
    let no_errors = BTreeMap::new();
    let rerender = |errors: &BTreeMap<String, String>, banner: Option<&str>, locked: bool| {
        let instance = FormInstance {
            post_url: format!("/admin/_action/{MODULE}/save"),
            form_id: FORM_ID.to_string(),
            posted: Some(&profile_body),
            errors,
            banner: banner.map(str::to_string),
            locked,
        };
        let rendered = render::panel_form_result(&frame, &profile_spec, &chrome, instance);
        render::fragment(env, "primitives.html", "panel_fragment", &rendered)
    };
    push!(plain("panel_form_rerender_422_chrome", rerender(&field_errors, None, false)));
    push!(plain(
        "panel_form_rerender_409_locked",
        rerender(&no_errors, Some("Someone else saved this form first."), true),
    ));
    // A form with modes shown again draws the mode its body posted picked, with that mode's
    // submit label.
    let moded_spec = moded_form();
    let moded_body = posted(&[("_mode", "revoke"), ("player", ""), ("reason", "")]);
    let moded_errors = BTreeMap::from([("player".to_string(), "is required".to_string())]);
    let moded = FormInstance {
        post_url: format!("/admin/_action/{MODULE}/grant"),
        form_id: FORM_ID.to_string(),
        posted: Some(&moded_body),
        errors: &moded_errors,
        banner: None,
        locked: false,
    };
    let rendered = render::panel_form_result(&frame, &moded_spec, &chrome, moded);
    let moded_rerender = render::fragment(env, "primitives.html", "panel_fragment", &rendered);
    push!(plain("panel_form_rerender_mode_revoke", moded_rerender));

    // ---- slot placeholders: loading_block and the denied states, by draw mode ----
    for (name, mode, denied) in [
        ("slot_panel_loading", "panel", false),
        ("slot_panel_denied", "panel", true),
        ("slot_tile_loading", "tile", false),
        ("slot_tile_denied", "tile", true),
        ("slot_attention_loading", "attention", false),
        ("slot_attention_denied", "attention", true),
        ("slot_inline_loading", "fact", false),
    ] {
        push!(plain(name, render::fragment(env, "primitives.html", "slot", &slot(mode, denied))));
    }
    let untitled = SlotView { title: String::new(), ..slot("panel", false) };
    push!(plain("slot_untitled", render::fragment(env, "primitives.html", "slot", &untitled)));

    // ---- modals ----
    let modal = |d: &ActionDecl, spec: ModalSpec<'_>| {
        let view = render::modal(
            d,
            spec,
            format!("/admin/_action/{MODULE}/{}", d.id),
            CSRF.to_string(),
            FORM_ID.to_string(),
            None,
            &no_errors,
        );
        render::fragment(env, "modal.html", "modal", &view)
    };
    let ban_spec = ConfirmSpec {
        title: "Ban p-1001?".to_string(),
        body: "The player is signed out and cannot sign in.".to_string(),
        tone: ConfirmTone::Danger,
        impact: vec![
            ("Sessions".to_string(), cell(CellKind::Number, "3")),
            ("Status".to_string(), cell(CellKind::Badge, "banned")),
            ("Owner".to_string(), cell(CellKind::User, "anna.kowalska")),
        ],
        reason: ReasonReq::Required,
        reason_hint: Some("e.g. chargeback fraud, ticket #".to_string()),
        type_to_confirm: Some("p-1001".to_string()),
        confirm_label: "Ban".to_string(),
    };
    let ban = ActionDecl { danger: true, ..decl("ban", "Ban player", "", Then::Confirm(ban_spec.clone())) };
    push!(plain("modal_confirm_danger_impact_reason", modal(&ban, ModalSpec::Confirm(&ban_spec))));
    let pause_spec = ConfirmSpec {
        body: "Deliveries stop until resumed.".to_string(),
        reason: ReasonReq::Optional,
        confirm_label: "Pause".to_string(),
        ..confirm("Pause webhooks?", ConfirmTone::Warn)
    };
    let pause = decl("pause", "Pause queue", "pause", Then::Confirm(pause_spec.clone()));
    push!(plain("modal_confirm_warn_optional_reason", modal(&pause, ModalSpec::Confirm(&pause_spec))));
    let notice_spec = ConfirmSpec {
        body: "Players see the notice on their next sign-in.".to_string(),
        confirm_label: "Send".to_string(),
        ..confirm("Send the notice?", ConfirmTone::Info)
    };
    let notice = decl("notice", "Send notice", "", Then::Confirm(notice_spec.clone()));
    push!(plain("modal_confirm_info_no_reason", modal(&notice, ModalSpec::Confirm(&notice_spec))));
    let grant_spec = FormSpec {
        fields: vec![
            Field { required: true, ..field("item_id", "Item", FieldKind::Ref("item".to_string()), None) },
            field("qty", "Quantity", FieldKind::Int, Some("1")),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Grant".to_string(),
        note: Some("Granted items appear in the next sync.".to_string()),
        revision: None,
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    };
    let grant = decl("grant", "Grant item", "gift", Then::Form(grant_spec.clone()));
    push!(plain("modal_form", modal(&grant, ModalSpec::Form(&grant_spec))));
    let moded_spec = moded_form();
    let adjust = decl("adjust", "Grant or revoke", "coins", Then::Form(moded_spec.clone()));
    push!(plain("modal_form_modes", modal(&adjust, ModalSpec::Form(&moded_spec))));
    let rotate_spec = FormSpec {
        fields: vec![
            Field {
                mono: true,
                placeholder: Some("e.g. key-2026-10".to_string()),
                ..field("key_id", "Key id", FieldKind::String, None)
            },
            Field {
                mono: true,
                placeholder: Some("Why rotate now".to_string()),
                ..field("why", "Why", FieldKind::Text, None)
            },
            Field {
                placeholder: Some("handle or player id".to_string()),
                ..field("owner", "Owner", FieldKind::Ref("player".to_string()), None)
            },
        ],
        hidden: BTreeMap::new(),
        submit_label: "Rotate".to_string(),
        note: None,
        revision: None,
        title: Some("Rotate the signing key".to_string()),
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    };
    let rotate = decl("rotate", "Rotate key", "key", Then::Form(rotate_spec.clone()));
    push!(plain("modal_form_title_mono_placeholder", modal(&rotate, ModalSpec::Form(&rotate_spec))));
    // A modal shown again after a POST, through `render::modal_rerender` as
    // `routes::render_instance` calls it.
    let reshow = |d: &ActionDecl,
                  spec: ModalSpec<'_>,
                  body: &[(String, String)],
                  errors: &BTreeMap<String, String>,
                  banner: Option<&str>,
                  locked: bool| {
        let instance = FormInstance {
            post_url: format!("/admin/_action/{MODULE}/{}", d.id),
            form_id: FORM_ID.to_string(),
            posted: Some(body),
            errors,
            banner: banner.map(str::to_string),
            locked,
        };
        render::fragment(env, "modal.html", "result", &render::modal_rerender(d, spec, CSRF.to_string(), instance))
    };
    let grant_errors = BTreeMap::from([
        ("item_id".to_string(), "must not be empty".to_string()),
        ("qty".to_string(), "must be an integer".to_string()),
        ("stray".to_string(), "not a field".to_string()),
    ]);
    push!(plain(
        "modal_rerender_form_errors_locked",
        reshow(
            &grant,
            ModalSpec::Form(&grant_spec),
            &posted(&[("item_id", ""), ("qty", "x")]),
            &grant_errors,
            Some("Someone else saved this form first."),
            true,
        ),
    ));
    push!(plain(
        "modal_rerender_form_stray_error",
        reshow(
            &grant,
            ModalSpec::Form(&grant_spec),
            &posted(&[("item_id", "sword-7"), ("qty", "2")]),
            &grant_errors,
            None,
            false,
        ),
    ));
    let reason_errors = BTreeMap::from([("_reason".to_string(), "a reason is required".to_string())]);
    push!(plain(
        "modal_rerender_confirm_reason_error",
        reshow(&ban, ModalSpec::Confirm(&ban_spec), &posted(&[("_reason", "")]), &reason_errors, None, false),
    ));
    // A form's required change note, drawn after its fields with the error the empty one got.
    let noted_spec = FormSpec {
        reason: ReasonReq::Required,
        reason_label: Some("Change note".to_string()),
        reason_hint: Some("Why? (recorded in the audit log)".to_string()),
        ..grant_spec.clone()
    };
    let noted = decl("grant", "Grant item", "gift", Then::Form(noted_spec.clone()));
    push!(plain(
        "modal_rerender_form_reason_error",
        reshow(
            &noted,
            ModalSpec::Form(&noted_spec),
            &posted(&[("item_id", "sword-7"), ("qty", "2"), ("_reason", "")]),
            &reason_errors,
            None,
            false,
        ),
    ));
    let result = ActionResultView { modal: None, toast: Some("Queue paused".to_string()), tone: "info" };
    push!(plain("modal_result_toast", render::fragment(env, "modal.html", "result", &result)));
    let flash = FlashView {
        message: "API key created".to_string(),
        reveal: Some(RevealView {
            label: "Key gb_live_…".to_string(),
            value: "gb_live_0123456789abcdef".to_string(),
            hint: None,
            expires_at: Some("2026-12-01".to_string()),
            extra: None,
        }),
        tone: "ok",
    };
    push!(plain("reveal_secret", render::fragment(env, "modal.html", "reveal", &flash)));
    let extra_flash = FlashView {
        message: "API key created".to_string(),
        reveal: Some(render::reveal(&SecretReveal {
            label: "Key gb_live_…".to_string(),
            value: "gb_live_0123456789abcdef".to_string(),
            hint: None,
            expires_at: None,
            extra: Some("Scoped to the ops role; rotate it from the keys page.".to_string()),
        })),
        tone: "ok",
    };
    push!(plain("reveal_secret_extra", render::fragment(env, "modal.html", "reveal", &extra_flash)));

    // ---- row menu and search palette ----
    let row_entity = entity("player", "p-1001");
    let profile = decl("profile", "Open profile", "", Then::Link(Link::Entity(row_entity.clone())));
    // The same row menu with each module up, with economy down, and with a manifest source
    // that has never answered.
    let row_menu = |down: &dyn Fn(&str) -> bool, complete: bool| {
        let row_item = |module: &str, d: &ActionDecl, role: Role| {
            let d = ActionDecl { point: ActionPoint::RowAction("player".to_string()), ..d.clone() };
            routes::action_item(&operator(role), CSRF, module, &declaring(&d, true), &d, Some(&row_entity))
        };
        let menu_items = vec![
            row_item("players", &decl("kick", "Kick", "x", Then::Invoke), Role::Admin),
            row_item("players", &ban, Role::Admin),
            row_item("players", &profile, Role::Admin),
            row_item("economy", &grant, Role::Admin),
            row_item("economy", &ActionDecl { min_role: Role::Admin, ..grant.clone() }, Role::Viewer),
        ];
        let menu = MenuView { entity: row_entity.clone(), groups: render::menu_groups(menu_items, down), complete };
        render::fragment(env, "primitives.html", "menu", &menu)
    };
    push!(plain("menu_row_actions", row_menu(&|_| false, true)));
    push!(plain("menu_row_actions_down", row_menu(&|m| m == "economy", true)));
    push!(plain("menu_row_actions_incomplete", row_menu(&|_| false, false)));
    let hit = |title: &str, subtitle: &str, badge: Option<&str>| {
        render::search_hit(&SearchHit {
            link: Link::Entity(entity("player", subtitle)),
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            badge: badge.map(str::to_string),
        })
    };
    let search = SearchView::new(
        "ann".to_string(),
        String::new(),
        vec![
            SearchGroupView {
                module: "players".to_string(),
                label: "Players".to_string(),
                icon: "user".to_string(),
                hits: vec![hit("Anna Kowalska", "p-1001", Some("active")), hit("Annette", "p-2040", None)],
                error: None,
            },
            SearchGroupView {
                module: "billing".to_string(),
                label: "Invoices".to_string(),
                icon: "coins".to_string(),
                hits: Vec::new(),
                error: Some("deadline of 800ms elapsed".to_string()),
            },
        ],
    );
    push!(plain("search_results_and_error", render::fragment(env, "primitives.html", "search", &search)));
    let nothing = SearchView::new(
        "zzz".to_string(),
        String::new(),
        vec![SearchGroupView {
            module: "players".to_string(),
            label: "Players".to_string(),
            icon: "user".to_string(),
            hits: Vec::new(),
            error: None,
        }],
    );
    push!(plain("search_no_matches", render::fragment(env, "primitives.html", "search", &nothing)));

    // ---- each Point's slot through routes::slot; the dashboard, page and entity screens ----
    let tab = || Point::EntityTab {
        kind: "player".to_string(),
        tab: "items".to_string(),
        tab_label: "Items".to_string(),
        columns: 1,
        params: Vec::new(),
    };
    push!(plain("slot_every_point", {
        let points = [
            Point::Page("overview".to_string()),
            Point::DashboardTile,
            Point::DashboardAttention,
            Point::DashboardPanel,
            Point::DashboardWide,
            Point::EntityHeader("player".to_string()),
            tab(),
            Point::EntityTabCount { kind: "player".to_string(), tab: "items".to_string() },
            Point::EntitySummary("player".to_string()),
            Point::NavCounter,
        ];
        let roles = [Role::Viewer, Role::Operator, Role::Admin];
        let slots: Result<Vec<String>, minijinja::Error> = points
            .into_iter()
            .zip(roles.into_iter().cycle())
            .enumerate()
            .map(|(i, (point, role))| {
                let s = point_slot(&format!("c{i}"), point, role, false);
                render::fragment(env, "primitives.html", "slot", &s)
            })
            .collect();
        slots.map(|slots| slots.join("\n"))
    }));
    push!(plain("screen_dashboard_ranges", {
        let dashboard = DashboardView {
            shell: shell_view("Dashboard"),
            subtitle: "Golden deployment".to_string(),
            tiles: vec![point_slot("tile", Point::DashboardTile, Role::Viewer, false)],
            attention: vec![point_slot("attention", Point::DashboardAttention, Role::Viewer, false)],
            panels: vec![point_slot("panel", Point::DashboardPanel, Role::Operator, false)],
            wide: vec![point_slot("wide", Point::DashboardWide, Role::Admin, false)],
            ranges: render::ranges(TimeRange::H24),
        };
        screen(env, "dashboard", &dashboard)
    }));
    push!(plain("screen_dashboard_incomplete", {
        let dashboard = DashboardView {
            shell: ShellView { complete: false, ..shell_view("Dashboard") },
            subtitle: "Golden deployment".to_string(),
            tiles: Vec::new(),
            attention: Vec::new(),
            panels: Vec::new(),
            wide: Vec::new(),
            ranges: render::ranges(TimeRange::H24),
        };
        screen(env, "dashboard", &dashboard)
    }));
    // The whole page, uncut: the document head, the topbar's palette opener, the popovers and
    // the palette scrim. `tools/admindiff`'s crawl tests build every page of their fake portal
    // from it.
    push!(plain("screen_shell_page", {
        let page = PageView {
            shell: shell_view("Golden page"),
            module: MODULE.to_string(),
            title: "Golden page".to_string(),
            subtitle: String::new(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            rows: Vec::new(),
            missing: Vec::new(),
        };
        render::screen_page(env, "page", &page)
    }));
    let page_point = || Point::Page("overview".to_string());
    for (name, layout) in [
        ("screen_page_stack", Layout::Stack(strings(&["a", "b"]))),
        ("screen_page_grid21", Layout::Grid21(strings(&["a"]), strings(&["b", "c"]))),
        ("screen_page_grid2", Layout::Grid2(strings(&["a"]), strings(&["b"]))),
        (
            "screen_page_rows",
            Layout::Rows(vec![Layout::Stack(strings(&["a"])), Layout::Grid2(strings(&["b"]), strings(&["c"]))]),
        ),
    ] {
        push!(plain(name, {
            let export = ActionDecl {
                point: ActionPoint::PageHead("overview".to_string()),
                ..decl("export", "Export", "download", Then::Invoke)
            };
            let page = PageView {
                shell: shell_view("Golden page"),
                module: MODULE.to_string(),
                title: "Golden page".to_string(),
                subtitle: String::new(),
                head_actions: vec![action(MODULE, &export, Role::Viewer, true)],
                tiles: vec![point_slot("tile", page_point(), Role::Viewer, true)],
                rows: render::layout_rows(&layout)
                    .into_iter()
                    .map(|(layout, ids)| LayoutRow {
                        layout,
                        columns: ids
                            .into_iter()
                            .map(|ids| ids.iter().map(|id| point_slot(id, page_point(), Role::Viewer, false)).collect())
                            .collect(),
                    })
                    .collect(),
                missing: Vec::new(),
            };
            screen(env, "page", &page)
        }));
    }
    push!(plain("screen_entity", {
        let player = entity("player", "p-1001");
        let at = |id: &str, point: Point| {
            routes::slot(&operator(Role::Viewer), MODULE, &contribution(id, point, Role::Viewer), Some(&player), false)
        };
        let kick = ActionDecl {
            point: ActionPoint::HeaderAction("player".to_string()),
            ..decl("kick", "Kick", "x", Then::Invoke)
        };
        let view = EntityView {
            shell: shell_view("p-1001"),
            kind_label: "Player".to_string(),
            entity_id: "p-1001".to_string(),
            avatar_color: render::avatar_color("p-1001"),
            avatar_initials: render::initials("p-1001"),
            menu_url: Some("/admin/_menu/player/p-1001".to_string()),
            headers: vec![at("header", Point::EntityHeader("player".to_string()))],
            summaries: vec![at("summary", Point::EntitySummary("player".to_string()))],
            header_actions: vec![routes::action_item(
                &operator(Role::Viewer),
                CSRF,
                MODULE,
                &declaring(&kick, true),
                &kick,
                Some(&player),
            )],
            tabs: vec![TabView {
                id: "items".to_string(),
                label: "Items".to_string(),
                url: "/admin/e/player/p-1001?tab=items".to_string(),
                counts: vec![at("count", Point::EntityTabCount { kind: "player".to_string(), tab: "items".to_string() })],
                down: false,
                readonly: false,
            }],
            selected_tab: Some("items".to_string()),
            tab_grid: false,
            tab_tiles: Vec::new(),
            tab_panels: vec![at("items", tab())],
        };
        screen(env, "entity", &view)
    }));
    push!(plain("screen_entity_tab_tiles", {
        let player = entity("player", "p-1001");
        let at = |id: &str, point: Point| {
            routes::slot(&operator(Role::Viewer), MODULE, &contribution(id, point, Role::Viewer), Some(&player), false)
        };
        let tab_tile = || Point::EntityTabTile { kind: "player".to_string(), tab: "items".to_string() };
        let tab_view = |id: &str, label: &str, down: bool, readonly: bool| TabView {
            id: id.to_string(),
            label: label.to_string(),
            url: format!("/admin/e/player/p-1001?tab={id}"),
            counts: Vec::new(),
            down,
            readonly,
        };
        let view = EntityView {
            shell: shell_view("p-1001"),
            kind_label: "Player".to_string(),
            entity_id: "p-1001".to_string(),
            avatar_color: render::avatar_color("p-1001"),
            avatar_initials: render::initials("p-1001"),
            menu_url: None,
            headers: Vec::new(),
            summaries: Vec::new(),
            header_actions: Vec::new(),
            tabs: vec![tab_view("items", "Items", false, true), tab_view("history", "History", true, false)],
            selected_tab: Some("items".to_string()),
            tab_grid: false,
            tab_tiles: vec![at("stock", tab_tile()), at("value", tab_tile())],
            tab_panels: vec![at("items", tab()), at("ledger", tab())],
        };
        screen(env, "entity", &view)
    }));

    out.into_iter()
        .map(|r| {
            let raw = r.result.unwrap_or_else(|e| panic!("golden case {} failed to render: {e:#}", r.name));
            Case {
                name: r.name,
                golden: golden_form(&raw),
                raw,
                primitive: r.primitive,
                state: r.state,
                seen: r.seen,
                refs: r.refs,
                asked: r.asked,
                refused: r.refused,
            }
        })
        .collect()
}
