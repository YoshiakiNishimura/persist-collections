//! A local, volatile snapshot backend over Unix-domain sockets.

use crate::{Save, SnapshotFormat, Stored, decode_complete};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fmt,
    io::{self, Read, Write},
    marker::PhantomData,
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const PROTOCOL: &[u8; 4] = b"PCD\x01";
const EMPTY: u32 = u32::MAX;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const SAVED: u8 = 0;
const STALE: u8 = 1;
const FORMAT_MISMATCH: u8 = 2;
const MAX_FORMAT_ID_BYTES: usize = 64;

/// A snapshot codec with a stable identifier for daemon sessions.
pub trait DaemonFormat: SnapshotFormat {
    const ID: &'static str;
    const CHECKSUM_LAYERS: u8 = 0;
}

#[cfg(feature = "json")]
impl DaemonFormat for crate::Json {
    const ID: &'static str = "json";
}

#[cfg(feature = "msgpack")]
impl DaemonFormat for crate::MessagePack {
    const ID: &'static str = "msgpack";
}

#[cfg(feature = "postcard")]
impl DaemonFormat for crate::Postcard {
    const ID: &'static str = "postcard";
}

#[cfg(feature = "checksum")]
impl<F: DaemonFormat> DaemonFormat for crate::Checked<F> {
    const ID: &'static str = F::ID;
    const CHECKSUM_LAYERS: u8 = F::CHECKSUM_LAYERS + 1;
}

/// A daemon save failure that callers can inspect inside the returned I/O error.
#[derive(Debug)]
pub enum DaemonSaveError {
    Conflict,
    Indeterminate(io::Error),
}

impl fmt::Display for DaemonSaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => {
                formatter.write_str("daemon snapshot changed; reopen and retry the operation")
            }
            Self::Indeterminate(error) => write!(
                formatter,
                "daemon save outcome unknown; reopen and inspect the data before retrying: {error}"
            ),
        }
    }
}

impl std::error::Error for DaemonSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conflict => None,
            Self::Indeterminate(error) => Some(error),
        }
    }
}

/// An open daemon session. A successful save acknowledges an in-memory update.
///
/// The connection keeps the revision read at open. Concurrent updates can make
/// it stale, in which case callers must reopen before saving again.
pub struct DaemonSnapshot<F> {
    connection: Mutex<Option<UnixStream>>,
    format: PhantomData<fn() -> F>,
}

struct ValueSnapshot<F>(DaemonSnapshot<F>);

impl<F: DaemonFormat> DaemonSnapshot<F> {
    /// Open a vector, using the supplied value when the daemon has no snapshot.
    pub fn open_vec<T: DeserializeOwned>(
        path: impl AsRef<Path>,
        fallback: Vec<T>,
    ) -> io::Result<Stored<Vec<T>, Self>> {
        let (backend, value) = Self::open(path.as_ref())?;
        Ok(Stored::with_backend(value.unwrap_or(fallback), backend))
    }

    /// Open a fixed-length array, checking the length of any stored sequence.
    pub fn open_array<T: DeserializeOwned, const N: usize>(
        path: impl AsRef<Path>,
        fallback: [T; N],
    ) -> io::Result<Stored<[T; N], Self>> {
        let (backend, value) = Self::open::<Vec<T>>(path.as_ref())?;
        let data = match value {
            Some(values) => values.try_into().map_err(|values: Vec<T>| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected {N} array elements, found {}", values.len()),
                )
            })?,
            None => fallback,
        };
        Ok(Stored::with_backend(data, backend))
    }

    /// Open a Serde value, using the supplied value when the daemon has no snapshot.
    pub fn open_value<T: Serialize + DeserializeOwned>(
        path: &Path,
        fallback: T,
    ) -> io::Result<Stored<T, impl Save<T> + use<F, T>>> {
        let (backend, value) = Self::open(path)?;
        Ok(Stored::with_backend(
            value.unwrap_or(fallback),
            ValueSnapshot(backend),
        ))
    }

    fn open<T: DeserializeOwned>(path: &Path) -> io::Result<(Self, Option<T>)> {
        let mut stream = UnixStream::connect(path)?;
        let timeout = Some(Duration::from_secs(10));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let format = F::ID.as_bytes();
        if format.is_empty() || format.len() > MAX_FORMAT_ID_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid daemon format identifier length",
            ));
        }
        stream.write_all(PROTOCOL)?;
        stream.write_all(&[format.len() as u8, F::CHECKSUM_LAYERS])?;
        stream.write_all(format)?;
        let mut protocol = [0; PROTOCOL.len()];
        stream.read_exact(&mut protocol)?;
        if &protocol != PROTOCOL {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported daemon protocol",
            ));
        }
        let mut reply = [0];
        stream.read_exact(&mut reply)?;
        match reply[0] {
            SAVED => {}
            FORMAT_MISMATCH => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "daemon snapshot uses a different format",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid daemon handshake response",
                ));
            }
        }
        let len = read_length(&mut stream)?;
        let data = if len == EMPTY {
            None
        } else {
            let bytes = read_payload(&mut stream, len)?;
            Some(decode_complete::<F, T>(bytes.as_slice())?)
        };
        Ok((
            Self {
                connection: Mutex::new(Some(stream)),
                format: PhantomData,
            },
            data,
        ))
    }

    fn write<T: Serialize + ?Sized>(&self, data: &T) -> io::Result<()> {
        let mut payload = LimitedBuffer(Vec::new());
        F::encode(&mut payload, data)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| io::Error::other("daemon connection lock poisoned"))?;
        let mut stream = connection.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "reopen the daemon snapshot before saving",
            )
        })?;
        let mut reply = [0];
        send_payload(&mut stream, &payload.0)
            .and_then(|()| stream.read_exact(&mut reply))
            .map_err(|error| io::Error::other(DaemonSaveError::Indeterminate(error)))?;
        match reply[0] {
            SAVED => {
                *connection = Some(stream);
                Ok(())
            }
            STALE => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                DaemonSaveError::Conflict,
            )),
            _ => Err(io::Error::other(DaemonSaveError::Indeterminate(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid daemon save acknowledgment",
                ),
            ))),
        }
    }
}

impl<F: DaemonFormat, T: Serialize> Save<Vec<T>> for DaemonSnapshot<F> {
    fn save(&self, data: &Vec<T>) -> io::Result<()> {
        self.write(data)
    }
}

impl<F: DaemonFormat, T: Serialize, const N: usize> Save<[T; N]> for DaemonSnapshot<F> {
    fn save(&self, data: &[T; N]) -> io::Result<()> {
        self.write(data.as_slice())
    }
}

impl<F: DaemonFormat, T: Serialize> Save<T> for ValueSnapshot<F> {
    fn save(&self, data: &T) -> io::Result<()> {
        self.0.write(data)
    }
}

#[derive(Default)]
struct Snapshot {
    revision: u64,
    payload: Option<Arc<Vec<u8>>>,
    format: Option<FormatTag>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FormatTag {
    metadata: [u8; 2],
    id: [u8; MAX_FORMAT_ID_BYTES],
}

/// Serve one volatile snapshot on a bound local listener until the process exits.
///
/// Bind the listener inside a directory accessible only to trusted clients.
/// Snapshots are limited to 16 MiB, and the server accepts up to 16 connections.
pub fn serve(listener: UnixListener) -> io::Result<()> {
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    for connection in listener.incoming() {
        let stream = connection?;
        if Arc::strong_count(&snapshot) > MAX_CONNECTIONS {
            continue;
        }
        let snapshot = Arc::clone(&snapshot);
        thread::Builder::new().spawn(move || {
            let _ = serve_connection(stream, &snapshot);
        })?;
    }
    Ok(())
}

fn serve_connection(mut stream: UnixStream, snapshot: &Mutex<Snapshot>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut protocol = [0; PROTOCOL.len()];
    stream.read_exact(&mut protocol)?;
    if &protocol != PROTOCOL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported daemon protocol",
        ));
    }
    let mut metadata = [0; 2];
    stream.read_exact(&mut metadata)?;
    let len = usize::from(metadata[0]);
    if len == 0 || len > MAX_FORMAT_ID_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid daemon format identifier length",
        ));
    }
    let mut id = [0; MAX_FORMAT_ID_BYTES];
    stream.read_exact(&mut id[..len])?;
    let format = FormatTag { metadata, id };
    let (mut revision, payload, stored_format) = {
        let current = snapshot
            .lock()
            .map_err(|_| io::Error::other("daemon snapshot lock poisoned"))?;
        (current.revision, current.payload.clone(), current.format)
    };
    stream.write_all(PROTOCOL)?;
    if stored_format.is_some_and(|stored| stored != format) {
        return stream.write_all(&[FORMAT_MISMATCH]);
    }
    stream.write_all(&[SAVED])?;
    match payload {
        Some(payload) => send_payload(&mut stream, &payload)?,
        None => stream.write_all(&EMPTY.to_be_bytes())?,
    }

    loop {
        let len = read_length(&mut stream)?;
        let payload = read_payload(&mut stream, len)?;
        let applied = {
            let mut current = snapshot
                .lock()
                .map_err(|_| io::Error::other("daemon snapshot lock poisoned"))?;
            if current.revision == revision {
                revision = revision
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("daemon revision exhausted"))?;
                current.payload = Some(Arc::new(payload));
                current.revision = revision;
                current.format = Some(format);
                true
            } else {
                false
            }
        };
        if !applied {
            stream.write_all(&[STALE])?;
            return Ok(());
        }
        stream.write_all(&[SAVED])?;
    }
}

fn read_length(reader: &mut UnixStream) -> io::Result<u32> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    Ok(u32::from_be_bytes(length))
}

fn read_payload(reader: &mut UnixStream, length: u32) -> io::Result<Vec<u8>> {
    let length = length as usize;
    if length > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon snapshot exceeds 16 MiB",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn send_payload(writer: &mut UnixStream, data: &[u8]) -> io::Result<()> {
    writer.write_all(&(data.len() as u32).to_be_bytes())?;
    writer.write_all(data)
}

struct LimitedBuffer(Vec<u8>);

impl Write for LimitedBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > MAX_SNAPSHOT_BYTES - self.0.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon snapshot exceeds 16 MiB",
            ));
        }
        self.0.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
