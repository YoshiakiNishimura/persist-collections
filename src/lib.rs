#![doc = include_str!("../README.md")]

use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

/// A standard collection paired with a storage backend.
pub struct Stored<C, B = Memory> {
    data: C,
    backend: B,
}

/// A backend that keeps everything in memory.
pub struct Memory;

/// A backend that stores a full collection as a JSON array.
pub struct JsonSnapshot {
    path: PathBuf,
}

/// A backend that can save a collection on demand.
pub trait Save<C> {
    fn save(&self, data: &C) -> io::Result<()>;
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

impl JsonSnapshot {
    /// Bind a snapshot path. Relative paths are resolved against the current directory.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            path: std::path::absolute(path)?,
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
        let data = match self.load::<T>()? {
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

    fn load<T: DeserializeOwned>(&self) -> io::Result<Option<Vec<T>>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let values = serde_json::from_reader(BufReader::new(file))?;
        Ok(Some(values))
    }

    fn write<T: Serialize>(&self, data: &[T]) -> io::Result<()> {
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
            serde_json::to_writer(&mut writer, data)?;
            writer.flush()?;
        }
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        #[cfg(unix)]
        directory.sync_all()?;
        Ok(())
    }
}

impl<T: Serialize> Save<Vec<T>> for JsonSnapshot {
    fn save(&self, data: &Vec<T>) -> io::Result<()> {
        self.write(data)
    }
}

impl<T: Serialize, const N: usize> Save<[T; N]> for JsonSnapshot {
    fn save(&self, data: &[T; N]) -> io::Result<()> {
        self.write(data)
    }
}
