#[test]
fn ships_filesystem_module() {
    let module = outimage::stdlib::get("filesystem").expect("filesystem module should exist");
    assert_eq!(module.name, "filesystem");
    assert!(module.source.contains("procedure open"));
    assert!(module.source.contains("external"));
}

#[test]
fn lists_all_modules() {
    let modules = outimage::stdlib::modules();
    assert!(modules.iter().any(|module| module.name == "filesystem"));
    assert!(modules.iter().any(|module| module.name == "io"));
    assert!(modules.iter().any(|module| module.name == "environment"));
}

#[test]
fn unknown_module_returns_none() {
    assert!(outimage::stdlib::get("networking").is_none());
}

#[test]
fn ships_io_module() {
    let module = outimage::stdlib::get("io").expect("io module should exist");
    assert!(module.source.contains("OutText"));
}

#[test]
fn ships_environment_module_with_random_surface() {
    let module = outimage::stdlib::get("environment").expect("environment module");
    assert!(module.source.contains("procedure draw"));
    assert!(module.source.contains("procedure poisson"));
    assert!(module.source.contains("external"));
}
