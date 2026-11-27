#[cfg(not(unix))]
use persist_collections::SnapshotFormat as BackendFormat;
#[cfg(unix)]
use persist_collections::daemon::DaemonFormat as BackendFormat;
use persist_collections::{
    Checked, FileSnapshot, Json, MessagePack, Postcard, Save, SaveOutcomeUnknown, SnapshotFormat,
    Stored, backup_once,
};
use records::{Applied, Change, Records};
use std::{
    env, fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

mod records;
mod wal;

const USAGE: &str = "Usage:
  persist-collections [--format <format>] <file> (insert <text> | list | delete <id> | backup <new-file> | restore <backup-file>)
  persist-collections --strategy wal [--format <format>] <directory> (insert <text> | list | delete <id> | checkpoint | backup <new-file>)
  persist-collections daemon serve <socket>
  persist-collections --backend daemon [--format <format>] <socket> (insert <text> | list | delete <id> | backup <new-file>)
Backup and restore accept --backup-format <format> before the file or socket.
Delete exit codes: 0 deleted, 2 record missing, 3 outcome unknown, 1 other error.
Formats: json, msgpack, postcard, json-crc32, msgpack-crc32, postcard-crc32";

enum Command {
    Insert(String),
    List,
    Delete(u64),
    Backup(PathBuf),
    Restore(PathBuf),
    Checkpoint,
}

#[derive(Clone, Copy)]
enum Backend {
    File,
    Daemon,
}

#[derive(Clone, Copy)]
enum FileStrategy {
    Snapshot,
    Wal,
}

fn lock_snapshot(path: &Path) -> io::Result<File> {
    let lock_path = path.with_added_extension("lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    Ok(lock)
}

macro_rules! with_format {
    ($format:expr, $operation:ident($($arg:expr),*)) => {
        match $format {
            "json" => $operation::<Json>($($arg),*),
            "msgpack" => $operation::<MessagePack>($($arg),*),
            "postcard" => $operation::<Postcard>($($arg),*),
            "json-crc32" => $operation::<Checked<Json>>($($arg),*),
            "msgpack-crc32" => $operation::<Checked<MessagePack>>($($arg),*),
            "postcard-crc32" => $operation::<Checked<Postcard>>($($arg),*),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
        }
    };
}

fn run() -> io::Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        return writeln!(io::stdout().lock(), "{USAGE}");
    }
    let usage_error = || io::Error::new(io::ErrorKind::InvalidInput, USAGE);
    if let [backend, command, socket] = args.as_slice()
        && backend == "daemon"
        && command == "serve"
    {
        return serve_daemon(Path::new(socket));
    }
    let mut args = args.as_slice();
    let mut format = "json";
    let mut backup_format = None;
    let mut backend = Backend::File;
    let mut strategy = FileStrategy::Snapshot;
    while let [flag, value, rest @ ..] = args {
        match flag.to_str() {
            Some("--format") => format = value.to_str().ok_or_else(usage_error)?,
            Some("--backup-format") => {
                backup_format = Some(value.to_str().ok_or_else(usage_error)?)
            }
            Some("--backend") if value == "file" => backend = Backend::File,
            Some("--backend") if value == "daemon" => backend = Backend::Daemon,
            Some("--backend") => return Err(usage_error()),
            Some("--strategy") if value == "snapshot" => strategy = FileStrategy::Snapshot,
            Some("--strategy") if value == "wal" => strategy = FileStrategy::Wal,
            Some("--strategy") => return Err(usage_error()),
            _ => break,
        }
        args = rest;
    }
    let [path, command, operands @ ..] = args else {
        return Err(usage_error());
    };
    let command = match (command.to_str(), operands) {
        (Some("insert"), [text]) => Command::Insert(
            text.to_str()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "record text must be valid UTF-8",
                    )
                })?
                .to_owned(),
        ),
        (Some("list"), []) => Command::List,
        (Some("checkpoint"), []) => Command::Checkpoint,
        (Some("backup"), [destination]) => Command::Backup(PathBuf::from(destination)),
        (Some("restore"), [source]) => Command::Restore(PathBuf::from(source)),
        (Some("delete"), [id]) => Command::Delete(
            id.to_str()
                .and_then(|id| id.parse().ok())
                .filter(|&id| id != 0)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "ID must be a positive 64-bit integer",
                    )
                })?,
        ),
        _ => return Err(usage_error()),
    };

    if backup_format.is_some() && !matches!(command, Command::Backup(_) | Command::Restore(_)) {
        return Err(usage_error());
    }
    if (matches!(strategy, FileStrategy::Wal) && matches!(backend, Backend::Daemon))
        || (matches!(command, Command::Checkpoint) && !matches!(strategy, FileStrategy::Wal))
    {
        return Err(usage_error());
    }
    with_format!(
        format,
        run_on(
            backend,
            strategy,
            Path::new(path),
            command,
            format,
            backup_format.unwrap_or(format)
        )
    )
}

fn run_on<F: BackendFormat>(
    backend: Backend,
    strategy: FileStrategy,
    path: &Path,
    command: Command,
    format: &str,
    backup_format: &str,
) -> io::Result<()> {
    if matches!(backend, Backend::Daemon) {
        #[cfg(unix)]
        return run_on_daemon::<F>(path, command, backup_format);
        #[cfg(not(unix))]
        return serve_daemon(path);
    }
    if matches!(strategy, FileStrategy::Wal) {
        return run_on_wal::<F>(path, command, format, backup_format);
    }
    match command {
        Command::Backup(destination) => {
            return with_format!(
                backup_format,
                copy_records(|| read_records::<F>(path), &destination)
            );
        }
        Command::Restore(source) => {
            return copy_records::<F>(|| with_format!(backup_format, read_records(&source)), path);
        }
        _ => {}
    }
    let snapshot = FileSnapshot::<F>::new(path)?;
    let _lock = lock_snapshot(path)?;
    run_records(
        snapshot.open_value(Records::empty())?,
        command,
        commit_snapshot,
    )
}

fn run_on_wal<F: SnapshotFormat>(
    directory: &Path,
    command: Command,
    format: &str,
    backup_format: &str,
) -> io::Result<()> {
    if matches!(command, Command::Restore(_)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "restore requires the snapshot strategy",
        ));
    }
    let lock = lock_snapshot(&directory.join("store"))?;
    let (wal, records) = wal::Wal::<F>::open(directory, format)?;
    match command {
        Command::Checkpoint => wal.checkpoint(&records),
        Command::Backup(destination) => {
            if !wal.has_saved_state() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "WAL store has no saved snapshot",
                ));
            }
            let absolute = std::path::absolute(&destination)?;
            if let Some(parent) = absolute.parent()
                && parent.canonicalize()? == directory.canonicalize()?
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write the backup outside the WAL store directory",
                ));
            }
            drop(lock);
            with_format!(backup_format, copy_records(|| Ok(records), &destination))
        }
        command => run_records(Stored::memory(records), command, |change, _| {
            wal.commit(change)
        }),
    }
}

#[cfg(unix)]
fn serve_daemon(path: &Path) -> io::Result<()> {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };

    let path = std::path::absolute(path)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path needs a parent directory",
        )
    })?;
    let metadata = fs::metadata(parent)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "daemon socket needs a private parent directory (mode 0700)",
        ));
    }
    persist_collections::daemon::serve(UnixListener::bind(path)?)
}

#[cfg(unix)]
fn run_on_daemon<F: BackendFormat>(
    path: &Path,
    command: Command,
    backup_format: &str,
) -> io::Result<()> {
    match command {
        Command::Backup(destination) => {
            return with_format!(
                backup_format,
                copy_records(
                    || persist_collections::DaemonSnapshot::<F>::read_value(path),
                    &destination
                )
            );
        }
        Command::Restore(_) => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "restore requires a new file destination",
            ));
        }
        _ => {}
    }
    let state = persist_collections::DaemonSnapshot::<F>::open_value(path, Records::empty())?;
    run_records(state, command, commit_snapshot)
}

#[cfg(not(unix))]
fn serve_daemon(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon backend requires Unix-domain sockets on a Unix platform",
    ))
}

fn run_records<B>(
    state: Stored<Records, B>,
    command: Command,
    commit: impl for<'a> FnOnce(Change<&'a str>, &'a Stored<Records, B>) -> io::Result<()>,
) -> io::Result<()> {
    state.get().validate()?;

    match command {
        Command::Insert(text) => {
            let id = insert_once(state, commit)(text)?;
            writeln!(io::stdout().lock(), "{id}")?;
        }
        Command::List => {
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, state.get().restrict(..))?;
            writeln!(stdout)?;
        }
        Command::Delete(id) => {
            delete_once(state, commit)(id)?;
        }
        Command::Backup(_) | Command::Restore(_) | Command::Checkpoint => unreachable!(),
    }
    Ok(())
}

fn insert_once<B>(
    state: Stored<Records, B>,
    commit: impl for<'a> FnOnce(Change<&'a str>, &'a Stored<Records, B>) -> io::Result<()>,
) -> impl FnOnce(String) -> io::Result<u64> {
    move |text| {
        let change = state.get().prepare_insert(text)?;
        apply_once(state, change, commit)
            .map(|applied| applied.id())
            .map_err(|error| match error {
                ApplyOnceError::Apply(cause) | ApplyOnceError::Commit(cause) => cause,
            })
    }
}

enum ApplyOnceError {
    Apply(io::Error),
    Commit(io::Error),
}

fn apply_once<B>(
    mut state: Stored<Records, B>,
    change: Change<String>,
    commit: impl for<'a> FnOnce(Change<&'a str>, &'a Stored<Records, B>) -> io::Result<()>,
) -> Result<Applied, ApplyOnceError> {
    state
        .get_mut()
        .apply(change)
        .map_err(ApplyOnceError::Apply)
        .and_then(|applied| {
            let change = state.get().applied_change(&applied);
            commit(change, &state)
                .map_err(ApplyOnceError::Commit)
                .map(|()| applied)
        })
}

fn commit_snapshot<B: Save<Records>>(
    _change: Change<&str>,
    staged: &Stored<Records, B>,
) -> io::Result<()> {
    staged.save()
}

#[derive(Debug)]
enum DeleteProblem {
    NotFound(u64),
    OutcomeUnknown { id: u64, cause: io::Error },
}

impl fmt::Display for DeleteProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(formatter, "record {id} does not exist"),
            Self::OutcomeUnknown { id, cause } => {
                write!(
                    formatter,
                    "delete outcome unknown for record {id} ({cause})"
                )
            }
        }
    }
}

impl std::error::Error for DeleteProblem {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound(_) => None,
            Self::OutcomeUnknown { cause, .. } => Some(cause),
        }
    }
}

fn delete_once<B>(
    state: Stored<Records, B>,
    commit: impl for<'a> FnOnce(Change<&'a str>, &'a Stored<Records, B>) -> io::Result<()>,
) -> impl FnOnce(u64) -> io::Result<()> {
    move |id| {
        apply_once(state, Change::Delete { id }, commit)
            .map(|_| ())
            .map_err(|error| match error {
                ApplyOnceError::Apply(cause) if cause.kind() == io::ErrorKind::NotFound => {
                    io::Error::new(io::ErrorKind::NotFound, DeleteProblem::NotFound(id))
                }
                ApplyOnceError::Commit(cause) if save_outcome_unknown(&cause) => {
                    io::Error::other(DeleteProblem::OutcomeUnknown { id, cause })
                }
                ApplyOnceError::Apply(cause) | ApplyOnceError::Commit(cause) => cause,
            })
    }
}

fn save_outcome_unknown(error: &io::Error) -> bool {
    let Some(cause) = error.get_ref() else {
        return false;
    };
    #[cfg(unix)]
    if matches!(
        cause.downcast_ref::<persist_collections::daemon::DaemonSaveError>(),
        Some(persist_collections::daemon::DaemonSaveError::Indeterminate(
            _
        ))
    ) {
        return true;
    }
    cause.is::<SaveOutcomeUnknown>()
}

fn read_records<F: SnapshotFormat>(source: &Path) -> io::Result<Records> {
    let _lock = lock_snapshot(source)?;
    FileSnapshot::<F>::new(source)?.read_value()
}

fn copy_records<F: SnapshotFormat>(
    source: impl FnOnce() -> io::Result<Records>,
    destination: &Path,
) -> io::Result<()> {
    backup_once(source, |records| {
        records.validate()?;
        let _lock = lock_snapshot(destination)?;
        FileSnapshot::<F>::new(destination)?.create_value(records)
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("persist-collections: {error}");
            command_error_status(&error)
        }
    }
}

fn command_error_status(error: &io::Error) -> ExitCode {
    match error
        .get_ref()
        .and_then(|cause| cause.downcast_ref::<DeleteProblem>())
    {
        Some(DeleteProblem::NotFound(_)) => ExitCode::from(2),
        Some(DeleteProblem::OutcomeUnknown { .. }) => ExitCode::from(3),
        None => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FailingSave<'a>(&'a Cell<usize>);

    impl Save<Records> for FailingSave<'_> {
        fn save(&self, records: &Records) -> io::Result<()> {
            self.0.set(self.0.get() + 1);
            assert_eq!(records.next_id, 2);
            assert_eq!(records.restrict(..)[0].id, 1);
            Err(io::Error::other("save failed"))
        }
    }

    #[test]
    fn insert_calls_save_only_after_preparation_and_returns_its_error() {
        let saves = Cell::new(0);
        let mut exhausted = Records::empty();
        exhausted.next_id = u64::MAX;
        let insert = insert_once(
            Stored::with_backend(exhausted, FailingSave(&saves)),
            commit_snapshot,
        );
        assert_eq!(
            insert("note".into()).unwrap_err().to_string(),
            "record IDs exhausted"
        );
        assert_eq!(saves.get(), 0);

        let insert = insert_once(
            Stored::with_backend(Records::empty(), FailingSave(&saves)),
            commit_snapshot,
        );
        assert_eq!(
            insert("note".into()).unwrap_err().to_string(),
            "save failed"
        );
        assert_eq!(saves.get(), 1);
    }

    #[test]
    fn insert_passes_the_allocated_change_to_the_selected_commit_function() {
        let insert = insert_once(Stored::memory(Records::empty()), |change, staged| {
            let Change::Insert { id, text, next_id } = change else {
                panic!("expected an insert");
            };
            assert_eq!(id, 1);
            assert_eq!(text, "note");
            assert_eq!(next_id, 2);
            assert!(std::ptr::eq(
                text,
                staged.get().restrict(..)[0].text.as_str()
            ));
            Ok(())
        });
        assert_eq!(insert("note".into()).unwrap(), 1);
    }

    #[test]
    fn delete_reports_missing_without_committing_and_preserves_the_next_id_on_success() {
        let delete = delete_once(Stored::memory(Records::empty()), |_, _| {
            panic!("a missing ID must never reach commit")
        });
        let missing = delete(1).unwrap_err();
        assert_eq!(command_error_status(&missing), ExitCode::from(2));

        let mut records = Records::empty();
        records
            .apply(records.prepare_insert("note".into()).unwrap())
            .unwrap();
        let delete = delete_once(Stored::memory(records), |change, staged| {
            assert!(matches!(change, Change::Delete { id: 1 }));
            assert!(staged.get().restrict(..).is_empty());
            assert_eq!(staged.get().next_id, 2);
            Ok(())
        });
        assert!(delete(1).is_ok());
    }

    #[test]
    fn delete_distinguishes_unknown_commit_from_an_ordinary_missing_file() {
        let mut records = Records::empty();
        records
            .apply(records.prepare_insert("note".into()).unwrap())
            .unwrap();
        let delete = delete_once(Stored::memory(records), |_, _| {
            Err(io::Error::other(SaveOutcomeUnknown::new(io::Error::other(
                "sync failed",
            ))))
        });
        let unknown = delete(1).unwrap_err();
        assert_eq!(command_error_status(&unknown), ExitCode::from(3));

        let mut records = Records::empty();
        records
            .apply(records.prepare_insert("note".into()).unwrap())
            .unwrap();
        let delete = delete_once(Stored::memory(records), |_, _| {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "parent directory disappeared",
            ))
        });
        let failed = delete(1).unwrap_err();
        assert_eq!(command_error_status(&failed), ExitCode::FAILURE);
        assert_eq!(failed.to_string(), "parent directory disappeared");
    }
}
