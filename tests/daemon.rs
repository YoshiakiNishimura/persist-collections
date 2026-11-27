#![cfg(all(
    unix,
    feature = "json",
    feature = "msgpack",
    feature = "postcard",
    feature = "checksum"
))]

use persist_collections::{DaemonSnapshot, Json, MessagePack, daemon::DaemonSaveError};
use persist_collections::{PostcardSnapshot, backup_once};
use serde_json::{Value, json};
use std::{
    fs, io,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    socket: PathBuf,
}

impl Server {
    fn start(socket: &Path) -> Self {
        let child = cli().args(["daemon", "serve"]).arg(socket).spawn().unwrap();
        let mut server = Self {
            child,
            socket: socket.into(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if UnixStream::connect(socket).is_ok() {
                return server;
            }
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "daemon exited before binding"
            );
            assert!(Instant::now() < deadline, "daemon did not become ready");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stop(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        fs::remove_file(&self.socket).unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_persist-collections"))
}

fn invoke(path: &Path, format: &str, args: &[&str]) -> Output {
    cli()
        .args(["--backend", "daemon", "--format", format])
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}

fn success(path: &Path, format: &str, args: &[&str]) -> Vec<u8> {
    let output = invoke(path, format, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn socket_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[test]
fn cli_invocations_share_memory_and_restart_empties_it() {
    let dir = socket_dir();
    let socket = dir.path().join("store.sock");
    let server = Server::start(&socket);

    assert_eq!(
        success(&socket, "postcard-crc32", &["insert", "最初"]),
        b"1\n"
    );
    let list = success(&socket, "postcard-crc32", &["list"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&list).unwrap(),
        json!([{"id": 1, "text": "最初"}])
    );
    let mismatch = invoke(&socket, "postcard", &["list"]);
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("different format"));
    success(&socket, "postcard-crc32", &["delete", "1"]);
    assert_eq!(
        success(&socket, "postcard-crc32", &["insert", "next"]),
        b"2\n"
    );
    let second_server = cli()
        .args(["daemon", "serve"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!second_server.status.success());
    server.stop();

    let _server = Server::start(&socket);
    assert_eq!(success(&socket, "postcard-crc32", &["list"]), b"[]\n");
}

#[test]
fn stale_sessions_and_sessions_from_a_stopped_process_cannot_overwrite() -> io::Result<()> {
    let dir = socket_dir();
    let socket = dir.path().join("store.sock");
    let server = Server::start(&socket);
    let mut stale = DaemonSnapshot::<Json>::open_vec(&socket, vec![1])?;
    let mut current = DaemonSnapshot::<Json>::open_vec(&socket, vec![2])?;
    current.save()?;
    current.get_mut().push(3);
    current.save()?;
    stale.get_mut().push(4);
    let conflict = stale.save().unwrap_err();
    assert!(matches!(
        conflict.get_ref().and_then(|error| error.downcast_ref()),
        Some(DaemonSaveError::Conflict)
    ));
    assert_eq!(
        DaemonSnapshot::<Json>::open_vec(&socket, Vec::<i32>::new())?.get(),
        &[2, 3]
    );
    server.stop();

    let _server = Server::start(&socket);
    let unknown = current.save().unwrap_err();
    assert!(matches!(
        unknown.get_ref().and_then(|error| error.downcast_ref()),
        Some(DaemonSaveError::Indeterminate(_))
    ));
    assert!(
        DaemonSnapshot::<Json>::open_vec(&socket, Vec::<i32>::new())?
            .get()
            .is_empty()
    );
    Ok(())
}

#[test]
fn large_fixed_arrays_round_trip_with_the_same_save_api() -> io::Result<()> {
    let dir = socket_dir();
    let socket = dir.path().join("store.sock");
    let _server = Server::start(&socket);
    let mut values = DaemonSnapshot::<MessagePack>::open_array(&socket, [0_u32; 64])?;
    values.get_mut()[63] = 42;
    values.save()?;
    assert_eq!(
        DaemonSnapshot::<MessagePack>::open_array(&socket, [1_u32; 64])?.get(),
        values.get()
    );
    assert!(DaemonSnapshot::<MessagePack>::open_array(&socket, [0_u32; 32]).is_err());
    let archive = PostcardSnapshot::new(dir.path().join("array.postcard"))?;
    backup_once(
        || DaemonSnapshot::<MessagePack>::read_value::<Vec<u32>>(&socket),
        |snapshot| archive.create_value(snapshot),
    )?;
    assert_eq!(archive.read_value::<Vec<u32>>()?, values.get().as_slice());
    Ok(())
}

#[test]
fn daemon_backup_can_use_another_format_and_restore_after_shutdown() {
    let dir = socket_dir();
    let socket = dir.path().join("store.sock");
    let archive = dir.path().join("backup.postcard");
    let restored = dir.path().join("restored.msgpack");
    let server = Server::start(&socket);
    let backup = || {
        cli()
            .args(["--backend", "daemon", "--backup-format", "postcard-crc32"])
            .arg(&socket)
            .arg("backup")
            .arg(&archive)
            .output()
            .unwrap()
    };

    assert!(!backup().status.success());
    assert!(!archive.exists());
    assert_eq!(success(&socket, "json", &["insert", "remove me"]), b"1\n");
    assert_eq!(success(&socket, "json", &["insert", "keep me"]), b"2\n");
    success(&socket, "json", &["delete", "1"]);
    let created = backup();
    assert!(created.status.success(), "{:?}", created);
    let original = fs::read(&archive).unwrap();
    assert!(!backup().status.success());
    assert_eq!(fs::read(&archive).unwrap(), original);
    server.stop();

    let output = cli()
        .args(["--format", "msgpack", "--backup-format", "postcard-crc32"])
        .arg(&restored)
        .arg("restore")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let list = cli()
        .args(["--format", "msgpack"])
        .arg(&restored)
        .arg("list")
        .output()
        .unwrap();
    assert!(list.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&list.stdout).unwrap(),
        json!([{"id": 2, "text": "keep me"}])
    );
    let inserted = cli()
        .args(["--format", "msgpack"])
        .arg(&restored)
        .args(["insert", "after restore"])
        .output()
        .unwrap();
    assert!(inserted.status.success());
    assert_eq!(inserted.stdout, b"3\n");
}
