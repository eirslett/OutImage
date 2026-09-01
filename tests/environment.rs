//! Chapter 9 — The Class ENVIRONMENT.

mod common;

#[test]
fn basic_ops_fixture_runs() {
    let source = common::fixture("environment/basic_ops.sim");
    let output = outimage::compile_str(&source).expect("basic ops fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn text_utils_fixture_runs() {
    let source = common::fixture("environment/text_utils.sim");
    let output = outimage::compile_str(&source).expect("text utils fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn math_fixture_runs() {
    let source = common::fixture("environment/math.sim");
    let output = outimage::compile_str(&source).expect("math fixture");
    assert_eq!(output, "   1.414    0.000    1.000    0.785\n");
}

#[test]
fn extremum_fixture_runs() {
    let source = common::fixture("environment/extremum.sim");
    let output = outimage::compile_str(&source).expect("extremum fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn constants_fixture_runs() {
    let source = common::fixture("environment/constants.sim");
    let output = outimage::compile_str(&source).expect("constants fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn array_bounds_fixture_runs() {
    let source = common::fixture("environment/array_bounds.sim");
    let output = outimage::compile_str(&source).expect("array bounds fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn random_fixture_runs() {
    let source = common::fixture("environment/random.sim");
    let output = outimage::compile_str(&source).expect("random fixture");
    assert_eq!(output, "F    2    5.388    1.192    0.007 0\n");
}

#[test]
fn histo_fixture_runs() {
    let source = common::fixture("environment/histo.sim");
    let output = outimage::compile_str(&source).expect("histo fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn current_attrs_fixture_runs() {
    let source = common::fixture("environment/current_attrs.sim");
    let output = outimage::compile_str(&source).expect("current attrs fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn sourceline_fixture_runs() {
    let source = common::fixture("environment/sourceline.sim");
    let output = outimage::compile_str(&source).expect("sourceline fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn datetime_fixture_runs() {
    let source = common::fixture("environment/datetime.sim");
    let output = outimage::compile_str(&source).expect("datetime fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn distributions_fixture_runs() {
    let source = common::fixture("environment/distributions.sim");
    let output = outimage::compile_str(&source).expect("distributions fixture");
    assert_eq!(output, "ok\n");
}

#[test]
fn error_fixture_stops() {
    let source = common::fixture("environment/error.sim");
    let error = outimage::compile_str(&source).expect_err("error should stop");
    assert!(
        error.to_string().contains("boom"),
        "diagnostic should include message: {error}"
    );
}

#[test]
fn antithetic_drawings_are_complements() {
    let source = r#"begin
        integer U, V;
        real a, b;
        boolean ok;
        U := 17;
        V := -17;
        a := uniform(0.0, 1.0, U);
        b := uniform(0.0, 1.0, V);
        ok := V < 0 and abs((a + b) - 1.0) < 1.0&-12;
        if ok then OutText("ok") else OutText("fail");
        OutImage;
    end"#;
    let output = outimage::compile_str(source).expect("antithetic");
    assert_eq!(output, "ok\n");
}

#[test]
fn environment_fixtures_compile() {
    for name in [
        "basic_ops.sim",
        "text_utils.sim",
        "math.sim",
        "extremum.sim",
        "constants.sim",
        "array_bounds.sim",
        "random.sim",
        "histo.sim",
        "datetime.sim",
        "distributions.sim",
        "current_attrs.sim",
        "sourceline.sim",
    ] {
        let source = common::fixture(&format!("environment/{name}"));
        outimage::compile_str(&source).unwrap_or_else(|error| {
            panic!("environment fixture {name} should compile: {error}");
        });
    }
}
