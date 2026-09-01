use outimage::CompileTarget;

#[test]
fn lists_wasm_targets() {
    assert!(CompileTarget::all().contains(&CompileTarget::WasmNode));
    assert!(CompileTarget::all().contains(&CompileTarget::WasmBrowser));
}

#[test]
fn native_target_has_sensible_triple() {
    let triple = CompileTarget::Native.triple();
    assert!(!triple.is_empty());
    assert!(triple.contains('-'));
}
