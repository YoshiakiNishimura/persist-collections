use crate::SnapshotFormat;
use crc32fast::Hasher;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, Read, Write},
    marker::PhantomData,
};

const HEADER: &[u8; 5] = b"PCSC\x01";

/// Wrap a snapshot format with a versioned header and a CRC-32 over its payload.
pub struct Checked<F>(PhantomData<fn() -> F>);

impl<F: SnapshotFormat> SnapshotFormat for Checked<F> {
    fn decode<T: DeserializeOwned>(mut reader: impl Read) -> io::Result<T> {
        let mut header = [0; HEADER.len()];
        reader.read_exact(&mut header)?;
        if &header != HEADER {
            return Err(invalid("invalid checksummed snapshot header or version"));
        }

        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let (mut payload, trailer) = bytes
            .split_last_chunk::<4>()
            .ok_or_else(|| invalid("missing snapshot checksum"))?;
        if crc32fast::hash(payload) != u32::from_le_bytes(*trailer) {
            return Err(invalid("snapshot checksum mismatch"));
        }

        let value = F::decode(&mut payload)?;
        if !payload.is_empty() {
            return Err(invalid("trailing snapshot payload"));
        }
        Ok(value)
    }

    fn encode<T: Serialize + ?Sized>(mut writer: impl Write, data: &T) -> io::Result<()> {
        writer.write_all(HEADER)?;
        let mut checksum = ChecksumWriter {
            writer: &mut writer,
            hasher: Hasher::new(),
        };
        F::encode(&mut checksum, data)?;
        let trailer = checksum.hasher.finalize().to_le_bytes();
        writer.write_all(&trailer)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct ChecksumWriter<W> {
    writer: W,
    hasher: Hasher,
}

impl<W: Write> Write for ChecksumWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let len = self.writer.write(data)?;
        self.hasher.update(&data[..len]);
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
