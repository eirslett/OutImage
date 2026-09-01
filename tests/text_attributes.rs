//! Chapter 8 — Attributes Of Text.

mod common;

#[test]
fn text_metadata_fixture_runs() {
    let source = common::fixture("text_attributes/text_metadata.sim");
    let output = outimage::compile_str(&source).expect("text metadata fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn character_access_fixture_runs() {
    let source = common::fixture("text_attributes/character_access.sim");
    let output = outimage::compile_str(&source).expect("character access fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn blanks_copy_fixture_runs() {
    let source = common::fixture("text_attributes/blanks_copy.sim");
    let output = outimage::compile_str(&source).expect("blanks/copy fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn sub_strip_fixture_runs() {
    let source = common::fixture("text_attributes/sub_strip.sim");
    let output = outimage::compile_str(&source).expect("sub/strip fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn deedit_edit_fixture_runs() {
    let source = common::fixture("text_attributes/deedit_edit.sim");
    let output = outimage::compile_str(&source).expect("deedit/edit fixture");
    assert_eq!(output.trim(), "186 900.00");
}

#[test]
fn overlapping_subtext_fixture_runs() {
    let source = common::fixture("text_attributes/overlapping_subtext.sim");
    let output = outimage::compile_str(&source).expect("overlapping/subtext fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn compact_fixture_runs() {
    let source = common::fixture("text_attributes/compact.sim");
    let output = outimage::compile_str(&source).expect("compact fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn text_attribute_fixtures_compile() {
    for name in [
        "text_metadata.sim",
        "character_access.sim",
        "blanks_copy.sim",
        "sub_strip.sim",
        "deedit_edit.sim",
        "overlapping_subtext.sim",
        "compact.sim",
    ] {
        let source = common::fixture(&format!("text_attributes/{name}"));
        outimage::compile_str(&source).unwrap_or_else(|error| {
            panic!("text attribute fixture {name} should compile: {error}");
        });
    }
}
