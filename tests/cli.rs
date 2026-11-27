use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn cli(path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_persist-collections"));
    command.arg(path);
    command
}

fn run(path: &Path, args: &[&str]) -> Output {
    cli(path).args(args).output().unwrap()
}

fn success(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = run(path, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn list(path: &Path) -> Value {
    serde_json::from_slice(&success(path, &["list"])).unwrap()
}

#[test]
fn records_and_ids_survive_separate_invocations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.json");
    assert_eq!(list(&path), json!([]));
    assert!(!path.exists());
    assert_eq!(success(&path, &["insert", "first note"]), b"1\n");
    assert_eq!(success(&path, &["insert", "second\nline"]), b"2\n");
    assert_eq!(
        list(&path),
        json!([
            {"id": 1, "text": "first note"},
            {"id": 2, "text": "second\nline"}
        ])
    );

    assert!(success(&path, &["delete", "1"]).is_empty());
    assert_eq!(list(&path), json!([{"id": 2, "text": "second\nline"}]));
    success(&path, &["delete", "2"]);
    assert_eq!(list(&path), json!([]));
    assert_eq!(success(&path, &["insert", "third note"]), b"3\n");
}

#[test]
fn invalid_commands_and_invalid_snapshots_preserve_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.json");
    success(&path, &["insert", "saved"]);
    let previous = fs::read(&path).unwrap();
    assert!(!run(&path, &["delete", "99"]).status.success());
    assert!(!run(&path, &["delete", "0"]).status.success());
    assert!(!run(&path, &["list", "extra"]).status.success());
    assert_eq!(fs::read(&path).unwrap(), previous);

    for corrupt in ["{", r#"{"next_id":1,"records":[{"id":1,"text":"saved"}]}"#] {
        fs::write(&path, corrupt).unwrap();
        assert!(!run(&path, &["insert", "new"]).status.success());
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupt);
    }
}

#[test]
fn overlapping_inserts_get_distinct_ids() {
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.json");
    let children: Vec<_> = (0..8)
        .map(|_| {
            cli(&path)
                .args(["insert", "note"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let ids: Vec<_> = list(&path)
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>());
}
