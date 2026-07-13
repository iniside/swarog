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
