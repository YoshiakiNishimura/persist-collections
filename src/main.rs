use persist_collections::JsonSnapshot;
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    process::ExitCode,
};

const USAGE: &str = "Usage: persist-collections <file> (insert <text> | list | delete <id>)";

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
    let [path, command, operands @ ..] = args.as_slice() else {
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

    let path = Path::new(path);
    let _lock = lock_snapshot(path)?;
    let mut state = JsonSnapshot::new(path)?.open_value(Records::empty())?;
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
    }
    Ok(())
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
