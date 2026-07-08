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

// --- Property test (port of Go's TestPropCodecRoundTrip in edge/prop_test.go) ---
use proptest::prelude::*;

proptest! {
    /// `decode(encode(v)) == v` for an arbitrary generated struct.
    #[test]
    fn prop_codec_roundtrip(a in any::<i32>(), b in "[a-zA-Z0-9 ]{0,16}") {
        let c = default_codec();
        let want = V { a, b };
        let data = c.encode(&want).unwrap();
        let got: V = c.decode(&data).unwrap();
        prop_assert_eq!(got, want);
    }
}
