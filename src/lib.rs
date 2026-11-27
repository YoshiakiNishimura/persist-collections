#![doc = include_str!("../README.md")]

use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
};

/// A standard collection paired with a storage backend.
pub struct Stored<C, B = Memory> {
    data: C,
    backend: B,
}

/// A backend that keeps everything in memory.
pub struct Memory;

/// A backend that writes full snapshots using format `F`.
pub struct FileSnapshot<F> {
    path: PathBuf,
    format: PhantomData<fn() -> F>,
}

struct ValueSnapshot<F>(FileSnapshot<F>);

/// Encodes and decodes snapshot values. Decoding must consume one complete snapshot.
pub trait SnapshotFormat {
    fn decode<T: DeserializeOwned>(reader: impl Read) -> io::Result<T>;
    fn encode<T: Serialize + ?Sized>(writer: impl Write, data: &T) -> io::Result<()>;
}

#[cfg(feature = "checksum")]
mod checksum;
#[cfg(feature = "checksum")]
pub use checksum::Checked;

#[cfg(unix)]
pub mod daemon;
#[cfg(unix)]
pub use daemon::DaemonSnapshot;

#[cfg(feature = "json")]
/// JSON snapshot format.
pub struct Json;

#[cfg(feature = "json")]
/// File backend for JSON snapshots.
pub type JsonSnapshot = FileSnapshot<Json>;

#[cfg(feature = "json")]
impl SnapshotFormat for Json {
    fn decode<T: DeserializeOwned>(reader: impl Read) -> io::Result<T> {
        Ok(serde_json::from_reader(reader)?)
    }

    fn encode<T: Serialize + ?Sized>(writer: impl Write, data: &T) -> io::Result<()> {
        Ok(serde_json::to_writer(writer, data)?)
    }
}

#[cfg(feature = "msgpack")]
/// MessagePack snapshot format.
pub struct MessagePack;

#[cfg(feature = "msgpack")]
/// File backend for MessagePack snapshots.
pub type MessagePackSnapshot = FileSnapshot<MessagePack>;

#[cfg(feature = "msgpack")]
impl SnapshotFormat for MessagePack {
    fn decode<T: DeserializeOwned>(reader: impl Read) -> io::Result<T> {
        rmp_serde::from_read(reader).map_err(|error| match error {
            rmp_serde::decode::Error::InvalidMarkerRead(error)
            | rmp_serde::decode::Error::InvalidDataRead(error) => error,
            error => io::Error::new(io::ErrorKind::InvalidData, error),
        })
    }

    fn encode<T: Serialize + ?Sized>(mut writer: impl Write, data: &T) -> io::Result<()> {
        rmp_serde::encode::write_named(&mut writer, data).map_err(|error| match error {
            rmp_serde::encode::Error::InvalidValueWrite(error) => error.into(),
            error => io::Error::new(io::ErrorKind::InvalidData, error),
        })
    }
}

#[cfg(feature = "postcard")]
const POSTCARD_HEADER: &[u8; 5] = b"PCSN\x01";

#[cfg(feature = "postcard")]
/// Postcard snapshot format with a versioned file header.
pub struct Postcard;

#[cfg(feature = "postcard")]
/// File backend for Postcard snapshots.
pub type PostcardSnapshot = FileSnapshot<Postcard>;

#[cfg(feature = "postcard")]
impl SnapshotFormat for Postcard {
    fn decode<T: DeserializeOwned>(mut reader: impl Read) -> io::Result<T> {
        let mut header = [0; POSTCARD_HEADER.len()];
        reader.read_exact(&mut header)?;
        if &header != POSTCARD_HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Postcard snapshot header or version",
            ));
        }

        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let (value, remaining) = postcard::take_from_bytes(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !remaining.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing snapshot data",
            ));
        }
        Ok(value)
    }

    fn encode<T: Serialize + ?Sized>(mut writer: impl Write, data: &T) -> io::Result<()> {
        writer.write_all(POSTCARD_HEADER)?;
        postcard::to_io(data, writer).map_err(io::Error::other)?;
        Ok(())
    }
}

/// A backend that can save a collection on demand.
///
/// Each backend defines what state a successful save confirms.
pub trait Save<C> {
    fn save(&self, data: &C) -> io::Result<()>;
}

/// Capture one complete snapshot and write it to a backup destination.
///
/// The source provides a consistent snapshot. The destination defines how
/// that snapshot is encoded and saved. The destination runs only after a
/// successful read, so the source can release any lock before writing starts.
pub fn backup_once<T>(
    read_snapshot: impl FnOnce() -> io::Result<T>,
    write_backup: impl FnOnce(&T) -> io::Result<()>,
) -> io::Result<()> {
    let snapshot = read_snapshot()?;
    write_backup(&snapshot)
}

impl<C> Stored<C, Memory> {
    /// Wrap a collection without requiring serialization.
    pub fn memory(data: C) -> Self {
        Self::with_backend(data, Memory)
    }
}

impl<C, B> Stored<C, B> {
    /// Pair an existing collection with a backend without loading any data.
    pub fn with_backend(data: C, backend: B) -> Self {
        Self { data, backend }
    }

    pub fn get(&self) -> &C {
        &self.data
    }

    pub fn get_mut(&mut self) -> &mut C {
        &mut self.data
    }

    pub fn into_inner(self) -> C {
        self.data
    }
}

impl<C, B: Save<C>> Stored<C, B> {
    /// Save the current collection using its backend.
    pub fn save(&self) -> io::Result<()> {
        self.backend.save(&self.data)
    }
}

impl<C> Save<C> for Memory {
    fn save(&self, _: &C) -> io::Result<()> {
        Ok(())
    }
}

impl<F: SnapshotFormat> FileSnapshot<F> {
    /// Bind a snapshot path. Relative paths are resolved against the current directory.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            path: std::path::absolute(path)?,
            format: PhantomData,
        })
    }

    /// Load a vector, or use the supplied vector if the file is missing.
    pub fn open_vec<T: DeserializeOwned>(
        self,
        fallback: Vec<T>,
    ) -> io::Result<Stored<Vec<T>, Self>> {
        let data = self.load()?.unwrap_or(fallback);
        Ok(Stored::with_backend(data, self))
    }

    /// Load an array of exactly N elements, or use the supplied array if the file is missing.
    pub fn open_array<T: DeserializeOwned, const N: usize>(
        self,
        fallback: [T; N],
    ) -> io::Result<Stored<[T; N], Self>> {
        let data = match self.load::<Vec<T>>()? {
            Some(values) => values.try_into().map_err(|values: Vec<T>| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected {N} array elements, found {}", values.len()),
                )
            })?,
            None => fallback,
        };
        Ok(Stored::with_backend(data, self))
    }

    /// Load a Serde value, or use the supplied value if the file is missing.
    pub fn open_value<T: Serialize + DeserializeOwned>(
        self,
        fallback: T,
    ) -> io::Result<Stored<T, impl Save<T>>> {
        let data = self.load()?.unwrap_or(fallback);
        Ok(Stored::with_backend(data, ValueSnapshot(self)))
    }

    /// Read an existing snapshot. A missing file returns an error.
    pub fn read_value<T: DeserializeOwned>(&self) -> io::Result<T> {
        self.load()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot does not exist: {}", self.path.display()),
            )
        })
    }

    /// Save a value to a new snapshot, failing if the destination already exists.
    pub fn create_value<T: Serialize + ?Sized>(&self, data: &T) -> io::Result<()> {
        self.write_with(data, false, |_| Ok(()))
    }

    fn load<T: DeserializeOwned>(&self) -> io::Result<Option<T>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        decode_complete::<F, T>(BufReader::new(file)).map(Some)
    }

    fn write<T: Serialize + ?Sized>(&self, data: &T) -> io::Result<()> {
        self.write_with(data, true, |_| Ok(()))
    }

    fn write_with<T: Serialize + ?Sized>(
        &self,
        data: &T,
        replace: bool,
        checkpoint: impl Fn(SavePoint) -> io::Result<()>,
    ) -> io::Result<()> {
        let parent = self.path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot path needs a parent directory",
            )
        })?;
        #[cfg(unix)]
        let directory = File::open(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            F::encode(&mut writer, data)?;
            writer.flush()?;
        }
        checkpoint(SavePoint::Written)?;
        temp.as_file().sync_all()?;
        checkpoint(SavePoint::FileSynced)?;
        if replace {
            temp.persist(&self.path).map_err(|error| error.error)?;
        } else {
            temp.persist_noclobber(&self.path)
                .map_err(|error| error.error)?;
        }
        checkpoint(SavePoint::Published)?;
        #[cfg(unix)]
        directory.sync_all()?;
        Ok(())
    }
}

fn decode_complete<F: SnapshotFormat, T: DeserializeOwned>(
    mut reader: impl BufRead,
) -> io::Result<T> {
    let value = F::decode(&mut reader)?;
    if !reader.fill_buf()?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing snapshot data",
        ));
    }
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SavePoint {
    Written,
    FileSynced,
    Published,
}

#[cfg(all(test, feature = "json"))]
mod recovery_tests;

impl<T: Serialize, F: SnapshotFormat> Save<Vec<T>> for FileSnapshot<F> {
    fn save(&self, data: &Vec<T>) -> io::Result<()> {
        self.write(data)
    }
}

impl<T: Serialize, const N: usize, F: SnapshotFormat> Save<[T; N]> for FileSnapshot<F> {
    fn save(&self, data: &[T; N]) -> io::Result<()> {
        self.write(data.as_slice())
    }
}

impl<T: Serialize, F: SnapshotFormat> Save<T> for ValueSnapshot<F> {
    fn save(&self, data: &T) -> io::Result<()> {
        self.0.write(data)
    }
}
