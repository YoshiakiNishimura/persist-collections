#[cfg(not(unix))]
use persist_collections::SnapshotFormat as BackendFormat;
#[cfg(unix)]
use persist_collections::daemon::DaemonFormat as BackendFormat;
use persist_collections::{
    Checked, FileSnapshot, Json, MessagePack, Postcard, Save, SnapshotFormat, Stored,
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
  persist-collections --backend daemon [--format <format>] <socket> (insert <text> | list | delete <id>)
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
    let mut format = Some("json");
    let mut backend = Backend::File;
    while let [flag, value, rest @ ..] = args {
        match flag.to_str() {
            Some("--format") => format = value.to_str(),
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

    match format {
        Some("json") => run_on::<Json>(backend, Path::new(path), command),
        Some("msgpack") => run_on::<MessagePack>(backend, Path::new(path), command),
        Some("postcard") => run_on::<Postcard>(backend, Path::new(path), command),
        Some("json-crc32") => run_on::<Checked<Json>>(backend, Path::new(path), command),
        Some("msgpack-crc32") => run_on::<Checked<MessagePack>>(backend, Path::new(path), command),
        Some("postcard-crc32") => run_on::<Checked<Postcard>>(backend, Path::new(path), command),
        _ => Err(usage_error()),
    }
}

fn run_on<F: BackendFormat>(backend: Backend, path: &Path, command: Command) -> io::Result<()> {
    if matches!(backend, Backend::Daemon) {
        #[cfg(unix)]
        return run_on_daemon::<F>(path, command);
        #[cfg(not(unix))]
        return serve_daemon(path);
    }
    match command {
        Command::Backup(destination) => return copy_snapshot::<F>(path, &destination),
        Command::Restore(source) => return copy_snapshot::<F>(&source, path),
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
fn run_on_daemon<F: BackendFormat>(path: &Path, command: Command) -> io::Result<()> {
    if matches!(command, Command::Backup(_) | Command::Restore(_)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "backup and restore require the file backend",
        ));
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

fn copy_snapshot<F: SnapshotFormat>(source: &Path, destination: &Path) -> io::Result<()> {
    let records: Records = {
        let _lock = lock_snapshot(source)?;
        FileSnapshot::<F>::new(source)?.read_value()?
    };
    records.validate()?;
    let _lock = lock_snapshot(destination)?;
    FileSnapshot::<F>::new(destination)?.create_value(&records)
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
