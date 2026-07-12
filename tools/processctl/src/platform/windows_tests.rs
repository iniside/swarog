use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

use super::windows::{command_line, environment_block};

#[test]
fn command_line_quotes_empty_spaces_quotes_and_trailing_slashes() {
    let args = vec![
        OsString::from(""),
        OsString::from("plain"),
        OsString::from("two words"),
        OsString::from("quote\"inside"),
        OsString::from("tail slash\\"),
    ];
    let encoded = command_line(Path::new(r"C:\Program Files\fixture.exe"), &args).unwrap();
    let rendered = OsString::from_wide(&encoded[..encoded.len() - 1])
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        rendered,
        r#""C:\Program Files\fixture.exe" "" plain "two words" "quote\"inside" "tail slash\\""#
    );
}

#[test]
fn environment_block_is_case_insensitively_sorted_and_double_terminated() {
    let mut environment = BTreeMap::new();
    environment.insert(OsString::from("z_key"), OsString::from("last"));
    environment.insert(OsString::from("A_KEY"), OsString::from("first"));
    let block = environment_block(&environment).unwrap();
    let rendered = OsString::from_wide(&block[..block.len() - 2])
        .to_string_lossy()
        .replace('\0', "|");
    assert_eq!(rendered, "A_KEY=first|z_key=last");
    assert_eq!(&block[block.len() - 2..], &[0, 0]);

    let empty = environment_block(&BTreeMap::new()).unwrap();
    assert_eq!(empty, vec![0, 0]);
}

#[test]
fn command_line_and_environment_reject_nul_injection() {
    assert!(command_line(
        Path::new("fixture.exe"),
        &[OsString::from_wide(&[b'x' as u16, 0])]
    )
    .is_err());
    let mut environment = BTreeMap::new();
    environment.insert(OsString::from("BAD=KEY"), OsString::from("value"));
    assert!(environment_block(&environment).is_err());
}
