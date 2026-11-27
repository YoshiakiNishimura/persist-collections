use super::{Change, Records};
use persist_collections::{
    Checked, FileSnapshot, JsonSnapshot, SaveOutcomeUnknown, SnapshotFormat,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::Path,
};

const FRAME_MAGIC: &[u8; 4] = b"PCW\x01";
const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
const LOG: &str = "changes.wal";
const CHECKPOINT: &str = "checkpoint";
const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct Manifest<T> {
    strategy: T,
    version: u8,
    codec: T,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint<T, C> {
    version: u8,
    codec: C,
    sequence: u64,
    records: T,
}

#[derive(Serialize, Deserialize)]
struct Entry<T> {
    sequence: u64,
    change: Change<T>,
}

/// The caller holds the store's sidecar lock throughout open and mutation.
pub(super) struct Wal<'a, F> {
    directory: &'a Path,
    codec: &'a str,
    initialized: bool,
    sequence: u64,
    log_bytes: u64,
    format: PhantomData<fn() -> F>,
}

impl<'a, F: SnapshotFormat> Wal<'a, F> {
    pub(super) fn has_saved_state(&self) -> bool {
        self.sequence != 0 || self.directory.join(CHECKPOINT).is_file()
    }

    pub(super) fn open(directory: &'a Path, codec: &'a str) -> io::Result<(Self, Records)> {
        let manifest = JsonSnapshot::new(directory.join("format"))?;
        let initialized = match manifest.read_value::<Manifest<String>>() {
            Ok(stored)
                if stored.strategy == "wal"
                    && stored.version == VERSION
                    && stored.codec == codec =>
            {
                true
            }
            Ok(_) => return Err(invalid("WAL store uses a different format or version")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if directory.join(CHECKPOINT).symlink_metadata().is_ok()
                    || directory.join(LOG).symlink_metadata().is_ok()
                {
                    return Err(invalid("WAL store is missing its format file"));
                }
                false
            }
            Err(error) => return Err(error),
        };

        let checkpoint = FileSnapshot::<Checked<F>>::new(directory.join(CHECKPOINT))?;
        let (mut sequence, mut records) =
            match checkpoint.read_value::<Checkpoint<Records, String>>() {
                Ok(saved) if saved.version == VERSION && saved.codec == codec => {
                    (saved.sequence, saved.records)
                }
                Ok(_) => return Err(invalid("WAL checkpoint uses a different format or version")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (0, Records::empty()),
                Err(error) => return Err(error),
            };

        let log_bytes = match File::open(directory.join(LOG)) {
            Ok(file) => replay::<F>(BufReader::new(file), &mut sequence, &mut records)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        Ok((
            Self {
                directory,
                codec,
                initialized,
                sequence,
                log_bytes,
                format: PhantomData,
            },
            records,
        ))
    }

    pub(super) fn commit(self, change: Change<&str>) -> io::Result<()> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("WAL sequence exhausted"))?;
        let mut payload = Vec::new();
        Checked::<F>::encode(&mut payload, &Entry { sequence, change })?;
        if payload.len() > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WAL record exceeds 16 MiB",
            ));
        }
        let len = payload.len() as u32;
        let mut header = [0; HEADER_LEN];
        header[..4].copy_from_slice(FRAME_MAGIC);
        header[4..8].copy_from_slice(&len.to_le_bytes());
        header[8..12].copy_from_slice(&(!len).to_le_bytes());

        self.initialize()?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.directory.join(LOG))?;
        if file.metadata()?.len() != self.log_bytes {
            file.set_len(self.log_bytes)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::Start(self.log_bytes))?;
        let result = (|| {
            file.write_all(&header)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            sync_directory(self.directory)
        })();
        result.map_err(|error| io::Error::other(SaveOutcomeUnknown::new(error)))
    }

    pub(super) fn checkpoint(self, records: &Records) -> io::Result<()> {
        self.initialize()?;
        FileSnapshot::<Checked<F>>::new(self.directory.join(CHECKPOINT))?.save_value(
            &Checkpoint {
                version: VERSION,
                codec: self.codec,
                sequence: self.sequence,
                records,
            },
        )?;

        // Recovery can safely ignore an old log after this checkpoint is published.
        // Retain the log on platforms where the file backend cannot sync the directory.
        #[cfg(unix)]
        match OpenOptions::new()
            .write(true)
            .open(self.directory.join(LOG))
        {
            Ok(file) => {
                file.set_len(0)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn initialize(&self) -> io::Result<()> {
        if !self.initialized {
            JsonSnapshot::new(self.directory.join("format"))?.create_value(&Manifest {
                strategy: "wal",
                version: VERSION,
                codec: self.codec,
            })?;
        }
        Ok(())
    }
}

fn replay<F: SnapshotFormat>(
    mut reader: impl Read,
    sequence: &mut u64,
    records: &mut Records,
) -> io::Result<u64> {
    let mut previous = None::<u64>;
    let mut bytes = 0;
    let mut header = [0; HEADER_LEN];
    while read_complete(&mut reader, &mut header)? {
        let len = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let inverse = u32::from_le_bytes(header[8..12].try_into().unwrap());
        if &header[..4] != FRAME_MAGIC || len != !inverse || len == 0 || len as usize > MAX_PAYLOAD
        {
            return Err(invalid("invalid WAL frame header"));
        }
        let mut payload = vec![0; len as usize];
        if !read_complete(&mut reader, &mut payload)? {
            break;
        }
        let entry: Entry<String> = Checked::<F>::decode(payload.as_slice())?;
        if entry.sequence == 0
            || previous.is_some_and(|prior| prior.checked_add(1) != Some(entry.sequence))
        {
            return Err(invalid("invalid WAL sequence"));
        }
        if entry.sequence > *sequence {
            if sequence.checked_add(1) != Some(entry.sequence) {
                return Err(invalid("WAL sequence gap after checkpoint"));
            }
            records
                .apply(entry.change)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            *sequence = entry.sequence;
        }
        previous = Some(entry.sequence);
        bytes += HEADER_LEN as u64 + u64::from(len);
    }
    Ok(bytes)
}

fn read_complete(reader: &mut impl Read, data: &mut [u8]) -> io::Result<bool> {
    match reader.read_exact(data) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
