#![cfg(all(
    feature = "json",
    feature = "msgpack",
    feature = "postcard",
    feature = "checksum"
))]

use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn wal(directory: &Path, format: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persist-collections"))
        .args(["--strategy", "wal", "--format", format])
        .arg(directory)
        .args(args)
        .output()
        .unwrap()
}

fn success(directory: &Path, format: &str, args: &[&str]) -> Vec<u8> {
    let output = wal(directory, format, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn list(directory: &Path, format: &str) -> Value {
    serde_json::from_slice(&success(directory, format, &["list"])).unwrap()
}

#[test]
fn wal_replays_changes_and_handles_a_checkpoint_with_its_old_log() {
    for format in ["json", "msgpack", "postcard", "postcard-crc32"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let log = path.join("changes.wal");
        let checkpoint = path.join("checkpoint");

        assert_eq!(list(path, format), json!([]));
        assert_eq!(success(path, format, &["insert", "first"]), b"1\n");
        assert_eq!(success(path, format, &["insert", "second"]), b"2\n");
        success(path, format, &["delete", "1"]);
        assert_eq!(list(path, format), json!([{"id":2,"text":"second"}]));
        assert!(!checkpoint.exists());
        let old_log = fs::read(&log).unwrap();
        assert!(!old_log.is_empty());
        assert!(!wal(path, "json-crc32", &["list"]).status.success());

        success(path, format, &["checkpoint"]);
        assert!(checkpoint.is_file());
        #[cfg(unix)]
        assert_eq!(fs::metadata(&log).unwrap().len(), 0);
        assert_eq!(list(path, format), json!([{"id":2,"text":"second"}]));

        // Simulate a stop after publishing the checkpoint and before reclaiming the log.
        fs::write(&log, &old_log).unwrap();
        assert_eq!(list(path, format), json!([{"id":2,"text":"second"}]));
        assert_eq!(success(path, format, &["insert", "third"]), b"3\n");
        success(path, format, &["delete", "2"]);
        success(path, format, &["checkpoint"]);
        assert_eq!(success(path, format, &["insert", "fourth"]), b"4\n");
        assert_eq!(
            list(path, format),
            json!([{"id":3,"text":"third"},{"id":4,"text":"fourth"}])
        );
    }
}

#[test]
fn wal_repairs_only_an_incomplete_tail_and_rejects_complete_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let log = path.join("changes.wal");
    success(path, "json", &["insert", "first"]);
    let first = fs::read(&log).unwrap();
    success(path, "json", &["insert", "incomplete"]);
    let mut truncated = fs::read(&log).unwrap();
    truncated.truncate(truncated.len() - 1);
    fs::write(&log, &truncated).unwrap();

    assert_eq!(list(path, "json"), json!([{"id":1,"text":"first"}]));
    assert_eq!(fs::read(&log).unwrap(), truncated);
    assert_eq!(success(path, "json", &["insert", "second"]), b"2\n");
    let mut complete = fs::read(&log).unwrap();
    assert!(complete.starts_with(&first));
    assert_eq!(
        list(path, "json"),
        json!([{"id":1,"text":"first"},{"id":2,"text":"second"}])
    );

    let last = complete.last_mut().unwrap();
    *last ^= 1;
    fs::write(&log, &complete).unwrap();
    let failure = wal(path, "json", &["insert", "refuse"]);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("checksum mismatch"));
    assert_eq!(fs::read(&log).unwrap(), complete);
}

#[test]
fn wal_backup_captures_the_checkpoint_and_later_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("wal-store");
    let archive = dir.path().join("backup.json");
    let restored = dir.path().join("restored.json");
    fs::create_dir(&store).unwrap();
    success(&store, "json", &["insert", "checkpointed"]);
    success(&store, "json", &["checkpoint"]);
    success(&store, "json", &["insert", "logged"]);
    success(&store, "json", &["delete", "1"]);

    let output = Command::new(env!("CARGO_BIN_EXE_persist-collections"))
        .args(["--strategy", "wal"])
        .arg(&store)
        .arg("backup")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = Command::new(env!("CARGO_BIN_EXE_persist-collections"))
        .arg(&restored)
        .arg("restore")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let state: Value = serde_json::from_slice(&fs::read(restored).unwrap()).unwrap();
    assert_eq!(
        state,
        json!({"records":[{"id":2,"text":"logged"}],"next_id":3})
    );
}
