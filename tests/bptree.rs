#![cfg(all(
    feature = "json",
    feature = "msgpack",
    feature = "postcard",
    feature = "checksum"
))]

use serde_json::{Value, json};
use std::{fs, process::Command};

#[test]
fn cli_reads_and_writes_a_separate_cow_btree_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_persist-collections"))
            .args(["--strategy", "cow-btree", "--format", "postcard"])
            .arg(path)
            .args(args)
            .output()
            .unwrap()
    };
    for (id, text) in [(1, "first"), (2, "second"), (3, "third")] {
        let result = run(&["insert", text]);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, format!("{id}\n").as_bytes());
    }
    let value = |args: &[&str]| -> Value {
        let result = run(args);
        assert!(result.status.success(), "{result:?}");
        serde_json::from_slice(&result.stdout).unwrap()
    };
    assert_eq!(value(&["get", "2"]), json!({"id":2,"text":"second"}));
    assert_eq!(value(&["get", "9"]), Value::Null);
    assert_eq!(
        value(&["range", "2", "3"]),
        json!([{"id":2,"text":"second"},{"id":3,"text":"third"}])
    );
    assert!(run(&["delete", "2"]).status.success());
    let checkpoint = fs::read(path.join("root")).unwrap();
    let page_bytes = fs::metadata(path.join("pages")).unwrap().len();
    assert_eq!(run(&["delete", "2"]).status.code(), Some(2));
    assert_eq!(fs::read(path.join("root")).unwrap(), checkpoint);
    assert_eq!(fs::metadata(path.join("pages")).unwrap().len(), page_bytes);
    assert_eq!(
        value(&["list"]),
        json!([{"id":1,"text":"first"},{"id":3,"text":"third"}])
    );
    let other_strategy = Command::new(env!("CARGO_BIN_EXE_persist-collections"))
        .args(["--strategy", "wal"])
        .arg(path)
        .arg("list")
        .output()
        .unwrap();
    assert!(!other_strategy.status.success());
}
