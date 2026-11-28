# Lean model

From this directory, run `lean Model.lean` with the Lean 4.19.0 toolchain in
`lean-toolchain`. The model uses the Lean distribution and needs no Mathlib.

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

The B+ tree contract is an explicit hypothesis. The model does not verify the
Rust page traversal, codec, or crash recovery implementation.
