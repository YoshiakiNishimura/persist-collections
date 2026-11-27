use super::{JsonSnapshot, SavePoint};
use serde::{Deserialize, Serialize};
use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    process::{Command, Stdio},
};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    next_id: u64,
    ids: Vec<u64>,
}

fn state(next_id: u64) -> State {
    State {
        next_id,
        ids: (1..next_id).collect(),
    }
}

const POINTS: [(SavePoint, &str, u64); 3] = [
    (SavePoint::Written, "written", 3),
    (SavePoint::FileSynced, "synced", 3),
    (SavePoint::Published, "published", 4),
];

#[test]
fn save_errors_preserve_whole_snapshots() -> io::Result<()> {
    for (point, _, expected) in POINTS {
        let dir = tempfile::tempdir()?;
        let snapshot = JsonSnapshot::new(dir.path().join("records.json"))?;
        snapshot.create_value(&state(3))?;
        let result = snapshot.write_with(&state(4), true, |current| {
            if current == point {
                return Err(io::Error::other("injected save failure"));
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err().to_string(), "injected save failure");
        assert_eq!(
            snapshot.read_value::<State>()?,
            state(expected),
            "{point:?}"
        );
    }
    Ok(())
}

#[test]
fn process_termination_preserves_whole_snapshots() -> io::Result<()> {
    for (point, signal, expected) in POINTS {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("records.json");
        let snapshot = JsonSnapshot::new(&path)?;
        snapshot.create_value(&state(3))?;
        let mut child = Command::new(env::current_exe()?)
            .args(["--exact", "recovery_tests::crash_child", "--nocapture"])
            .env("PERSIST_COLLECTIONS_TEST_CRASH_POINT", signal)
            .env("PERSIST_COLLECTIONS_TEST_CRASH_PATH", &path)
            .stdout(Stdio::piped())
            .spawn()?;

        let ready = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .any(|line| line.is_ok_and(|line| line == "snapshot-checkpoint"));
        let kill_result = child.kill();
        let status = child.wait()?;
        assert!(ready, "child exited before {point:?}: {status}");
        kill_result?;
        assert!(!status.success());
        assert_eq!(
            snapshot.read_value::<State>()?,
            state(expected),
            "{point:?}"
        );
    }
    Ok(())
}

#[test]
fn crash_child() -> io::Result<()> {
    let Ok(signal) = env::var("PERSIST_COLLECTIONS_TEST_CRASH_POINT") else {
        return Ok(());
    };
    let (point, _, _) = POINTS.iter().find(|(_, name, _)| *name == signal).unwrap();
    let path = env::var_os("PERSIST_COLLECTIONS_TEST_CRASH_PATH").unwrap();
    JsonSnapshot::new(path)?.write_with(&state(4), true, |current| {
        if current == *point {
            // libtest may leave a partial line on stdout before running the test.
            writeln!(io::stdout(), "\nsnapshot-checkpoint")?;
            io::stdout().flush()?;
            loop {
                std::thread::park();
            }
        }
        Ok(())
    })
}
