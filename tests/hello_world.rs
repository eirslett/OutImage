mod common;

#[test]
fn compiles_hello_world_fixture() {
    let source = common::fixture("hello_world.sim");
    let output = outimage::compile_str(&source).expect("hello world program should compile");
    assert_eq!(output, "hello world\n");
}

#[test]
fn compiles_hello_world_case_insensitively() {
    let output = outimage::compile_str(r#"begin outtext("hello world"); outimage; end;"#)
        .expect("hello world program should compile");
    assert_eq!(output, "hello world\n");
}
