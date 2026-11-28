# Lean model

From this directory, check the models with the Lean 4.19.0 toolchain in
`lean-toolchain` (POSIX shell). The models need no Mathlib.

```sh
check_dir=$(mktemp -d)
LEAN_PATH="$check_dir" lean -o "$check_dir/Model.olean" Model.lean
LEAN_PATH="$check_dir" lean -o "$check_dir/BPlusTree.olean" BPlusTree.lean
LEAN_PATH="$check_dir" lean -o "$check_dir/BPlusUpdate.olean" BPlusUpdate.lean
LEAN_PATH="$check_dir" lean -o "$check_dir/CowRecovery.olean" CowRecovery.lean
LEAN_PATH="$check_dir" lean CowHistory.lean
```

The state consists of rows in strictly ascending ID order and the next ID to
allocate. IDs are modeled as natural numbers with the Rust `u64` bound.
Insertion returns `none` when advancing the next ID would overflow. Deletion
returns `none` for a missing ID and preserves the allocation counter.

| Theorem | Guarantee |
| --- | --- |
| `apply_preserves_valid` | Successful updates preserve ID ordering, uniqueness, and allocation bounds |
| `restrict_and` | A conjunction selects the same ordered rows as two successive restrictions |
| `bounds_sound` | Every matching row with a valid ID lies inside the extracted inclusive interval |
| `range_pushdown` | Restricting to the extracted interval before checking the predicate preserves the result |
| `bplus_restrict` | A B+ tree range scan that satisfies the ordered range contract can take the place of the reference scan |

`BPlusTree.lean` models the successful range traversal in
[`src/bptree.rs`](../src/bptree.rs) over one fixed root. An internal page
visits children in fence order, starts at the last fence at or below the
inclusive lower bound (or at the first child), and stops above the inclusive
upper bound. A leaf drops rows below the lower bound and takes rows through
the upper bound.

| Theorem | Guarantee |
| --- | --- |
| `seek_eq_partition`, `leaf_drop_eq_partition` | Branch and leaf seeks agree with slicing at Rust's partition points on ordered inputs |
| `flatten_ordered` | Local leaf and fence invariants imply global strict ID order |
| `visit_eq_rangeRows` | Visiting one root yields exactly its in-range rows as a list, preserving values, multiplicity, and order |
| `visit_same_view` | If the root represents the logical view, the scan is complete for that view and strictly ordered by ID |
| `visit_reversed` | A reversed interval produces no rows |
| `scan_eq_rangeRows` | The public entry point has the same range semantics, including a missing root or reversed bounds |
| `restrict_visit` | The proven scan can supply the candidates for the existing predicate restriction |

The proof assumes sorted leaves, strictly increasing child fences, and every
child's keys lying at or above its own fence and below later fences. The model
holds the root fixed for the scan and assumes successful page reads and row
emission. Rust page updates, root publication, I/O failures, and corruption
handling are outside the proof.

`BPlusUpdate.lean` models successful updates to the same page type. It inserts
at the sorted leaf position, rejects duplicate IDs, and splits a leaf or branch
at the midpoint when it grows past 16 entries. Deletion removes a matching
row, discards pages that become empty, and collapses a singleton root branch
once. Surviving and newly split pages carry the exact first ID as their fence.
The model uses the depth of the resulting root, including after a root split or
collapse.

| Theorem | Guarantee |
| --- | --- |
| `editChildren_eq_partition` | An update rewrites the child selected by Rust's predecessor slot |
| `exact_valid` | Exact fences and ordered rows imply the earlier scan invariant |
| `updateRoot_spec` | A successful insert or delete preserves strict ID order and exact fences and changes only the requested row |
| `scan_after_update` | The earlier range scan is complete and strictly ID-ordered on the new root |

The update proof concerns the in-memory page model and the ordering and fence
invariants. It abstracts Rust's leaf binary search as a sorted-list operation.
It does not verify storage writes, publication, crash recovery, or page
occupancy bounds.

## Copy-on-write publication and recovery

`CowRecovery.lean` models the publication protocol in
[`src/cow_btree.rs`](../src/cow_btree.rs). New page records append at the
actual tail, past any unreferenced records left by an earlier attempt. Branch
links can reuse older pages and must point backward in the file. The manifest
publishes the root link, end position, allocation counter, version, and codec
together. A reader uses only that manifest's file prefix.

| Interruption point | Manifest that may survive |
| --- | --- |
| Append, page-file sync, or sync of the new page-file name | Old |
| Write and sync the temporary manifest | Old |
| Atomically replace the manifest, before the directory sync is confirmed | Old or new |
| Final directory sync has returned successfully | New |

A fresh store first publishes an empty manifest. On Unix, the first update
makes the new page-file name durable before replacing that manifest.
Synchronizing only the page file would leave open the case where a surviving
manifest names a tree in a missing page file.
[Linux `fsync(2)`](https://man7.org/linux/man-pages/man2/fsync.2.html)

| Theorem | Guarantee |
| --- | --- |
| `reopen_prefix` | Extending a durable file prefix cannot change the view read through its manifest |
| `crash_reopens_whole_version` | Every modeled interruption yields the complete old or new view, including its manifest and reachable tree |
| `confirmed_reopens_new` | After the final directory sync, only the new view can reopen |
| `bootstrap_reopens_empty` | Interruption during initial empty-manifest creation still exposes an empty store |
| `crash_after_update` | The same recovery result applies to the logical insert/delete model when the staged page image represents its result |

The theorem starts from one durable old manifest and a complete staged image
of the proposed new tree. It assumes a single writer, crash-atomic manifest
replacement, effective file and directory syncs, and preservation of already
durable bytes during append. The model uses decoded page records in place of
byte offsets and codecs. The proof covers one publication attempt with these
filesystem guarantees. Device corruption, recovery followed by another
commit on an unconfirmed manifest, and non-Unix power-loss behavior are
outside its scope.

## Repeated updates and restarts

`CowHistory.lean` starts with the durable empty manifest from the bootstrap
model and covers any finite sequence of page appends, manifest renames,
directory confirmations, process restarts, and power recoveries. It tracks the
last confirmed version separately from the currently visible version.
Generations are proof labels and are independent of record IDs and of the
on-disk format.

After an unconfirmed rename, a process restart reads the currently visible
manifest. A later power recovery may select any complete candidate published
since the last confirmed directory sync, including the confirmed version
itself. Merely reading a manifest after reboot leaves the set of candidates
unchanged. A successful directory confirmation makes the visible version the
new floor and the only candidate at that instant.

| Theorem | Guarantee |
| --- | --- |
| `Step.preserves` | Every possible manifest still opens its complete tree after each step, and the confirmed floor can only advance |
| `Trace.preserves` | The invariant and the confirmed floor hold across arbitrarily many steps |
| `restart_after_trace` | Recovery exposes one complete version at or above every earlier confirmed generation. Recovery at the confirmed generation yields exactly the confirmed view |
| `restart_from_empty` | The same result holds for histories beginning at the durable empty manifest |

Candidates share a durable append-only file, so older manifests retain every
page they can reference. The proof uses the single-attempt filesystem
assumptions above and a conservative recovery rule for multiple renames.
Between directory confirmations, any complete candidate can survive a power
loss. This is an assumption about filesystem recovery, and the proof does not
depend on a particular ordering of unconfirmed renames. Page reuse, compaction
that moves or overwrites pages, and filesystems that violate those guarantees
are outside the model.
