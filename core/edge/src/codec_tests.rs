use super::*;
use serde::Deserialize;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct V {
    a: i32,
    b: String,
}

#[test]
fn json_codec_roundtrips() {
    let c = default_codec();
    let v = V { a: 7, b: "x".into() };
    let bytes = c.encode(&v).unwrap();
    assert_eq!(bytes, br#"{"a":7,"b":"x"}"#.to_vec());
    let back: V = c.decode(&bytes).unwrap();
    assert_eq!(back, v);
}

#[test]
fn decode_error_is_codec_variant() {
    let c = default_codec();
    let err = c.decode::<V>(b"not json").unwrap_err();
    assert!(matches!(err, Error::Codec(_)));
}
