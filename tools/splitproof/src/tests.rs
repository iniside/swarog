use super::extract_form_fields;

#[test]
fn form_extractor_decodes_minijinja_attributes_once() {
    let html = r#"<form><input type="hidden" name="_expected_state" value="{&quot;name&quot;:&quot;dev&amp;ops&quot;,&quot;literal&quot;:&quot;&amp;quot;&quot;,&quot;path&quot;:&quot;a&#x2f;b&quot;,&quot;quote&quot;:&quot;&#x27;&quot;,&quot;tag&quot;:&quot;&lt;&gt;&quot;}"></form>"#;

    assert_eq!(
        extract_form_fields(html),
        vec![(
            "_expected_state".to_string(),
            r#"{"name":"dev&ops","literal":"&quot;","path":"a/b","quote":"'","tag":"<>"}"#
                .to_string(),
        )],
    );
}

/// The failing branch of the pre-spawn probe: a port that already has a listener
/// (a stale process from a previous hung run) must bail loudly, naming the port and
/// the service about to be spawned.
#[test]
fn stale_listener_probe_bails_when_the_port_is_already_bound() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();

    let error = super::ensure_no_stale_listener("probe-svc", port)
        .expect_err("occupied port must be rejected");
    let message = format!("{error:#}");
    assert!(
        message.contains(&format!(":{port}")) && message.contains("probe-svc"),
        "error must name the port and service: {message}"
    );

    // Positive control: once the listener is gone, the same port passes.
    drop(listener);
    super::ensure_no_stale_listener("probe-svc", port).expect("freed port must pass");
}

/// [LG1]'s once-wrong branch: a Player cell whose label never arrived. The portal then draws
/// the plain id as the cell text (core admin `ref_cell`); the predicate must refuse it, and
/// accept the same cell drawing the handle, with the uuid left only in the link. The markup is
/// the portal's user cell (`primitives.html.tmpl` `cell_one`), `/` escaped in the href as
/// minijinja draws it.
#[test]
fn ledger_player_cell_refuses_an_unlabelled_uuid() {
    use super::wallet_ledger_checks::player_cell_ok;
    let pid = "0b6f3c2e-7a41-4d8e-9c55-1f2a3b4c5d6e";
    let cell = |href: &str, initials: &str, value: &str| {
        format!(
            r#"<a href="{href}" style="color:inherit"><span class="user-cell"><span class="avatar" style="background:#6b8afd">{initials}</span><span>{value}</span></span></a>"#
        )
    };
    let link = format!("&#x2f;admin&#x2f;e&#x2f;player&#x2f;{pid}");

    assert!(player_cell_ok(&cell(&link, "L", "Ledger#1812"), pid, "Ledger#1812"));
    // The unlabelled draw: the uuid as the text.
    assert!(!player_cell_ok(&cell(&link, "0", pid), pid, "Ledger#1812"));
    // Another player's label, a link to another player, or no link at all is no pass either.
    assert!(!player_cell_ok(&cell(&link, "L", "Ledger#0001"), pid, "Ledger#1812"));
    assert!(!player_cell_ok(&cell("&#x2f;admin&#x2f;e&#x2f;player&#x2f;other", "L", "Ledger#1812"), pid, "Ledger#1812"));
    assert!(!player_cell_ok(r#"<span class="user-cell"><span>Ledger#1812</span></span>"#, pid, "Ledger#1812"));
    // The uuid outside the link, even in markup only, is refused.
    let leaked = cell(&link, "L", "Ledger#1812").replace(r#"class="user-cell""#, &format!(r#"class="user-cell" title="{pid}""#));
    assert!(!player_cell_ok(&leaked, pid, "Ledger#1812"));
}

#[test]
fn inventory_holding_read_takes_exactly_one_row() {
    use super::inventory_admin_checks::only;
    assert_eq!(only(vec!["coin"], "player holding").unwrap(), "coin");
    // A second holding fails the fixture instead of letting the first row pass (a fetch_one
    // would take it), and so does none.
    let two = only(vec!["coin", "sword"], "player holding").unwrap_err();
    assert!(two.to_string().contains("2 player holding rows"), "{two}");
    assert!(only(Vec::<&str>::new(), "character holding").is_err());
}
