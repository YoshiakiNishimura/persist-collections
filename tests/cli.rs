#![cfg(all(feature = "json", feature = "msgpack", feature = "postcard"))]

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
fn postcard_cli_persists_records_and_next_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.postcard");
    let run_postcard = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_persist-collections"))
            .args(["--format", "postcard"])
            .arg(&path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    assert_eq!(run_postcard(&["insert", "記録"]), b"1\n");
    let listed: Value = serde_json::from_slice(&run_postcard(&["list"])).unwrap();
    assert_eq!(listed, json!([{"id": 1, "text": "記録"}]));
    assert!(!run(&path, &["insert", "wrong format"]).status.success());
    assert!(run_postcard(&["delete", "1"]).is_empty());
    assert_eq!(run_postcard(&["insert", "next"]), b"2\n");
}

#[test]
fn explicit_messagepack_format_round_trips_and_rejects_json() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("records.msgpack");
    let run_binary = |path: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_persist-collections"))
            .args(["--format", "msgpack"])
            .arg(path)
            .args(args)
            .output()
            .unwrap()
    };

    let inserted = run_binary(&binary, &["insert", "記録"]);
    assert!(inserted.status.success(), "{:?}", inserted);
    assert_eq!(inserted.stdout, b"1\n");
    let data = fs::read(&binary).unwrap();
    assert_eq!(data[0], 0x82); // MessagePack map with two fields.
    assert!(!run(&binary, &["insert", "wrong format"]).status.success());
    assert_eq!(fs::read(&binary).unwrap(), data);
    let listed = run_binary(&binary, &["list"]);
    assert!(listed.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap(),
        json!([{"id": 1, "text": "記録"}])
    );
    assert!(run_binary(&binary, &["delete", "1"]).status.success());
    assert_eq!(run_binary(&binary, &["list"]).stdout, b"[]\n");

    let json = dir.path().join("records.json");
    success(&json, &["insert", "saved"]);
    let previous = fs::read(&json).unwrap();
    assert!(
        !run_binary(&json, &["insert", "wrong format"])
            .status
            .success()
    );
    assert_eq!(fs::read(&json).unwrap(), previous);
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
