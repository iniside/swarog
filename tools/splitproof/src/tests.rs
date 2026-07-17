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
