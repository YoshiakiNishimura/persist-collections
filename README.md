# persist-collections

Choose the data structure, storage backend, and file format separately. `Stored<C, B>` pairs a value with a backend. Use `get()` and `get_mut()` to access the original Rust type.

| Choice | Built in | Extension point |
| --- | --- | --- |
| Data structure | `Vec<T>`, `[T; N]`, and types supported by the selected backend | Pass your own Rust type as `C` |
| Storage backend | `Memory` and `FileSnapshot<F>` | Implement `Save<C>` and provide your own loading API |
| Snapshot format | JSON, MessagePack, and Postcard | Implement `SnapshotFormat` |

## In memory

```rust
use persist_collections::Stored;

let mut numbers = Stored::memory(vec![1, 2]);
numbers.get_mut().push(3);
numbers.save()?; // No-op for the memory backend.
assert_eq!(numbers.into_inner(), vec![1, 2, 3]);
# Ok::<(), std::io::Error>(())
```

Memory mode accepts elements without Serde traits.

## JSON snapshots

```rust
# #[cfg(feature = "json")] {
use persist_collections::JsonSnapshot;

let dir = tempfile::tempdir()?;
let path = dir.path().join("slots.json");
let mut slots = JsonSnapshot::new(&path)?.open_array([0, 0, 0])?;
slots.get_mut()[1] = 42;
slots.save()?;

let reopened = JsonSnapshot::new(&path)?.open_array([0, 0, 0])?;
assert_eq!(reopened.get(), &[0, 42, 0]);
# }
# Ok::<(), std::io::Error>(())
```

For vectors, call `open_vec(vec![1, 2])` with the initial value. Both methods use the initial value when opening the path returns `NotFound`. The first call to `save()` creates the file if its parent directory exists. Malformed JSON, deserialization errors, and a fixed-length array with the wrong number of elements return errors. Opening requires deserializable elements and saving requires serializable elements. The file stores a plain JSON array using Serde's JSON representation.

Each `save()` writes the entire collection to a temporary file in the same directory, syncs the file contents, and replaces the destination. On Unix, it then syncs the parent directory. If that last step fails, the destination may already contain the new snapshot even though save returned an error. On other platforms, the parent directory is left unsynced. Atomic replacement and power-loss durability depend on the filesystem. Saving replaces the destination entry, including a symlink, with a new file and can change file permissions. Use a path in a directory you control. There is no automatic save on drop or interprocess locking. Coordinate writers to the same path externally, including the read-modify-save cycle. Otherwise the last successful replacement wins.

Use the `Save<C>` trait and `Stored::with_backend` to add another backend. Loading is part of the backend's own API.

`open_value` also accepts a Serde type, such as a struct containing a collection and its metadata. The decoder reads directly into that type.

## Choose a snapshot format

`JsonSnapshot`, `MessagePackSnapshot`, and `PostcardSnapshot` provide the same `open_vec`, `open_array`, and `open_value` methods. They share `FileSnapshot<F>` for file I/O and replacement. Implement `SnapshotFormat` and use `FileSnapshot<YourFormat>` to add a codec. The format and collection types are resolved at compile time.

All three codecs are enabled by default. To use the library with only Postcard, specify `default-features = false, features = ["postcard"]` in Cargo. Postcard payloads use the [Postcard wire format](https://postcard.jamesmunns.com/wire-format) and are prefixed with a file-format version. They require the matching Rust schema on read. Plan a migration when changing the persisted schema. The Postcard decoder buffers the encoded snapshot in memory.

## Single-collection CLI

From the repository root, run:

```sh
cargo run -- records.json insert "first note"
cargo run -- records.json insert "second note"
cargo run -- records.json list
cargo run -- records.json delete 1
cargo run -- records.json list
```

`insert` prints a numeric ID. `list` prints a JSON array of records in ID order, or `[]` for a missing snapshot. `delete` exits successfully without output and reports an error for a missing ID. Each change saves the full snapshot. IDs survive restarts and are never reused during the lifetime of that snapshot, even after deleting all records.

The CLI stores the next ID and records together in the snapshot. Use a dedicated data path for it. CLI invocations using the same path coordinate through a persistent sidecar, such as `records.json.lock`. Leave that file in place and use a local filesystem with file-lock support. On WSL, keep the data on the Linux filesystem when using multiple invocations concurrently.

JSON is the default file format. The CLI builds with all three format features. To select a binary format, pass it on each invocation:

```sh
cargo run -- --format msgpack records.msgpack insert "first note"
cargo run -- --format msgpack records.msgpack list
cargo run -- --format postcard records.postcard insert "first note"
cargo run -- --format postcard records.postcard list
```

Choose the format when creating a store and use the same selection when reopening it. The CLI prints `list` as JSON for every file format. Existing JSON files continue to work with the default commands.

## Recovery and backup

The crash tests terminate a child process after writing the temporary file, after syncing it, and after publishing it. The save failure tests inject errors at the same checkpoints. A failure after publication can leave the complete new state visible, so reopen the file before retrying an update. The tests cover process termination and injected failures. Power-loss durability depends on the filesystem and platform. Unix syncs the parent directory after publication.

Create a backup and restore it to a separate path:

```sh
cargo run -- records.json backup /path/to/backup.json
cargo run -- recovered.json restore /path/to/backup.json
cargo run -- recovered.json list
```

Both commands validate the source, re-encode the CLI records and next ID, and require a new destination path. Restore preserves the records and next ID from the backup, so IDs issued after the backup may be reused in the recovered store. Keep backups on independent storage. To recover a damaged store, open the restored file after checking it and keep the original for diagnosis. Use the same `--format` for backup and restore as for normal operations. Tests include fixed snapshots for the original JSON, MessagePack, and Postcard layouts.

## Optional checksums

Use `FileSnapshot<Checked<YourFormat>>` in Rust or add `-crc32` to a CLI format, such as `--format postcard-crc32`. The wrapper verifies a CRC-32 over the encoded payload before decoding. It detects accidental changes, including altered values that still parse. The wrapper buffers the encoded payload on read and adds a versioned header and checksum on disk. Legacy snapshots use their original format names. CRC-32 provides accidental corruption detection.

## Verify

Run `cargo test --locked` from the repository root. GitHub Actions runs the suite on Linux, Windows, and macOS.

## License

MIT.
