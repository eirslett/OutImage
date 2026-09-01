mod common;

use outimage::runtime::fs;

#[test]
fn exists_returns_false_for_missing_path() {
    assert!(!fs::exists("/tmp/sim-does-not-exist"));
}

#[test]
fn read_and_write_round_trip() {
    let path = common::temp_path("read_write.sim");
    fs::write_file(&path, "hello simula").unwrap();

    assert!(fs::exists(&path));
    assert_eq!(fs::read_file(&path).unwrap(), "hello simula");

    std::fs::remove_file(path).unwrap();
}

#[test]
fn read_file_errors_when_missing() {
    let path = common::temp_path("missing.sim");
    let error = fs::read_file(&path).unwrap_err();
    assert!(error.to_string().contains("path not found"));
}

#[test]
fn list_dir_returns_entries() {
    let dir = common::temp_path("list_dir");
    std::fs::create_dir_all(&dir).unwrap();
    fs::write_file(&format!("{dir}/a.sim"), "a").unwrap();
    fs::write_file(&format!("{dir}/b.sim"), "b").unwrap();

    let mut entries = fs::list_dir(&dir).unwrap();
    entries.sort();

    assert_eq!(entries, vec!["a.sim".to_string(), "b.sim".to_string()]);

    std::fs::remove_dir_all(dir).unwrap();
}
