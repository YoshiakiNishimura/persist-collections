#[cfg(not(unix))]
use persist_collections::SnapshotFormat as BackendFormat;
#[cfg(unix)]
use persist_collections::daemon::DaemonFormat as BackendFormat;
use persist_collections::{
    Checked, FileSnapshot, Json, MessagePack, Postcard, Save, SnapshotFormat, Stored, backup_once,
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

const USAGE: &str = "Usage:
  persist-collections [--format <format>] <file> (insert <text> | list | delete <id> | backup <new-file> | restore <backup-file>)
  persist-collections daemon serve <socket>
  persist-collections --backend daemon [--format <format>] <socket> (insert <text> | list | delete <id> | backup <new-file>)
Backup and restore accept --backup-format <format> before the file or socket.
Formats: json, msgpack, postcard, json-crc32, msgpack-crc32, postcard-crc32";

#[derive(Deserialize, Serialize)]
struct Record {
    id: u64,
    text: String,
}

#[derive(Deserialize, Serialize)]
struct Records {
    next_id: u64,
    records: Vec<Record>,
}

impl Records {
    fn empty() -> Self {
        Self {
            next_id: 1,
            records: Vec::new(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        let mut last_id = 0;
        for record in &self.records {
            if record.id <= last_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record IDs must be strictly increasing",
                ));
            }
            last_id = record.id;
        }
        if self.next_id <= last_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "next_id must exceed all record IDs",
            ));
        }
        Ok(())
    }

    fn insert(&mut self, text: String) -> io::Result<u64> {
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("record IDs exhausted"))?;
        self.records.push(Record { id, text });
        Ok(id)
    }

    fn delete(&mut self, id: u64) -> io::Result<()> {
        let position = self
            .records
            .binary_search_by_key(&id, |record| record.id)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("record {id} does not exist"),
                )
            })?;
        self.records.remove(position);
        Ok(())
    }
}

enum Command {
    Insert(String),
    List,
    Delete(u64),
    Backup(PathBuf),
    Restore(PathBuf),
}

#[derive(Clone, Copy)]
enum Backend {
    File,
    Daemon,
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
    while let [flag, value, rest @ ..] = args {
        match flag.to_str() {
            Some("--format") => format = value.to_str().ok_or_else(usage_error)?,
            Some("--backup-format") => {
                backup_format = Some(value.to_str().ok_or_else(usage_error)?)
            }
            Some("--backend") if value == "file" => backend = Backend::File,
            Some("--backend") if value == "daemon" => backend = Backend::Daemon,
            Some("--backend") => return Err(usage_error()),
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
    with_format!(
        format,
        run_on(
            backend,
            Path::new(path),
            command,
            backup_format.unwrap_or(format)
        )
    )
}

fn run_on<F: BackendFormat>(
    backend: Backend,
    path: &Path,
    command: Command,
    backup_format: &str,
) -> io::Result<()> {
    if matches!(backend, Backend::Daemon) {
        #[cfg(unix)]
        return run_on_daemon::<F>(path, command, backup_format);
        #[cfg(not(unix))]
        return serve_daemon(path);
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
    run_records(snapshot.open_value(Records::empty())?, command)
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
    run_records(state, command)
}

#[cfg(not(unix))]
fn serve_daemon(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon backend requires Unix-domain sockets on a Unix platform",
    ))
}

fn run_records<B: Save<Records>>(
    mut state: Stored<Records, B>,
    command: Command,
) -> io::Result<()> {
    state.get().validate()?;

    match command {
        Command::Insert(text) => {
            let id = state.get_mut().insert(text)?;
            state.save()?;
            writeln!(io::stdout().lock(), "{id}")?;
        }
        Command::List => {
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, &state.get().records)?;
            writeln!(stdout)?;
        }
        Command::Delete(id) => {
            state.get_mut().delete(id)?;
            state.save()?;
        }
        Command::Backup(_) | Command::Restore(_) => unreachable!(),
    }
    Ok(())
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
            ExitCode::FAILURE
        }
    }
}
