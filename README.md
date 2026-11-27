# persist-collections

Keep Rust's standard `Vec<T>` and `[T; N]` and choose how to save them. `Stored` pairs the collection with a backend. Use `get()` and `get_mut()` to access the original Rust type.

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
use persist_collections::JsonSnapshot;

let dir = tempfile::tempdir()?;
let path = dir.path().join("slots.json");
let mut slots = JsonSnapshot::new(&path)?.open_array([0, 0, 0])?;
slots.get_mut()[1] = 42;
slots.save()?;

let reopened = JsonSnapshot::new(&path)?.open_array([0, 0, 0])?;
assert_eq!(reopened.get(), &[0, 42, 0]);
# Ok::<(), std::io::Error>(())
```

For vectors, call `open_vec(vec![1, 2])` with the initial value. Both methods use the initial value when opening the path returns `NotFound`. The first call to `save()` creates the file if its parent directory exists. Malformed JSON, deserialization errors, and a fixed-length array with the wrong number of elements return errors. Opening requires deserializable elements and saving requires serializable elements. The file stores a plain JSON array using Serde's JSON representation.

Each `save()` writes the entire collection to a temporary file in the same directory, syncs the file contents, and replaces the destination. On Unix, it then syncs the parent directory. If that last step fails, the destination may already contain the new snapshot even though save returned an error. On other platforms, the parent directory is left unsynced. Atomic replacement and power-loss durability depend on the filesystem. Saving replaces the destination entry, including a symlink, with a new file and can change file permissions. Use a path in a directory you control. There is no automatic save on drop or interprocess locking. Coordinate writers to the same path externally, including the read-modify-save cycle. Otherwise the last successful replacement wins.

Use the `Save<C>` trait and `Stored::with_backend` to add another backend. Loading is part of the backend's own API.

`JsonSnapshot::open_value` also accepts a Serde type, such as a struct containing a collection and its metadata. The decoder reads directly into that type. Collection and backend types are selected at compile time. The existing `open_vec` and `open_array` methods continue to use plain JSON arrays.

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

The CLI stores the next ID and records together in a JSON object. Use a dedicated data path for it. CLI invocations using the same path coordinate through a persistent sidecar, such as `records.json.lock`. Leave that file in place and use a local filesystem with file-lock support. On WSL, keep the data on the Linux filesystem when using multiple invocations concurrently.

## Verify

Run `cargo test --locked` from the repository root.

## License

MIT.
