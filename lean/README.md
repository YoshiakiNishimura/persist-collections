# Lean model

From this directory, check both models with the Lean 4.19.0 toolchain in
`lean-toolchain` (POSIX shell). The models need no Mathlib.

```sh
check_dir=$(mktemp -d)
LEAN_PATH="$check_dir" lean -o "$check_dir/Model.olean" Model.lean
LEAN_PATH="$check_dir" lean BPlusTree.lean
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
