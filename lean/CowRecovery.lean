import BPlusUpdate

namespace PersistCollections.BPlusTree.Cow

/-- A file position in the model counts complete records, abstracting byte offsets. -/
abbrev Address := Nat
abbrev Link := Nat × Address

inductive DiskPage where
  | leaf (rows : List Row)
  | branch (links : List Link)

def DiskPage.first : DiskPage → Option Nat
  | .leaf rows => rows.head?.map Row.id
  | .branch links => links.head?.map Prod.fst

/-- A slot with no decoded record can stand for a torn or unusable append. -/
abbrev PageFile := List (Option DiskPage)

structure Manifest where
  version : Nat
  codec : String
  nextId : Nat
  endPos : Nat
  tree : Option Link

structure Disk where
  manifest : Option Manifest
  pages : Option PageFile

structure View where
  manifest : Manifest
  tree : Option Page

def emptyManifest (codec : String) : Manifest := ⟨1, codec, 1, 0, none⟩

/-- The reader sees only the published base. Every branch edge points backward. -/
def readTree (base : PageFile) : Nat → Link → Option Page
  | 0, _ => none
  | depth + 1, (first, address) => do
    let record ← base[address]?.join
    if record.first != some first then none
    else match record with
      | .leaf rows => some (.leaf rows)
      | .branch links =>
        if links.all (fun link => decide (link.2 < address)) then
          let readChild (link : Link) := (readTree base depth link).map (link.1, ·)
          (links.mapM readChild).map Page.branch
        else none

def readView (depth : Nat) (m : Manifest) (file : Option PageFile) : Option View :=
  match m.tree with
  | none => some ⟨m, none⟩
  | some link => do
    let pages ← file
    if m.endPos ≤ pages.length then
      (readTree (pages.take m.endPos) depth link).map (fun tree => ⟨m, some tree⟩)
    else none

def reopen (depth : Nat) (codec : String) (disk : Disk) : Option View :=
  match disk.manifest with
  | none => match disk.pages with
    | none => some ⟨emptyManifest codec, none⟩
    | some _ => none
  | some m =>
    if m.version = 1 ∧ m.codec = codec ∧ 0 < m.nextId ∧
        (∀ link ∈ m.tree, 0 < link.1 ∧ link.1 < m.nextId ∧ link.2 < m.endPos) then
      readView depth m disk.pages
    else none

/-- A power loss may leave an arbitrary suffix after the previously durable prefix. -/
def Extends (durable crashed : Option PageFile) : Prop :=
  match durable with
  | none => True
  | some base => ∃ tail, crashed = some (base ++ tail)

private theorem extends_trans {a b c : Option PageFile} :
    Extends a b → Extends b c → Extends a c := by
  cases a with
  | none => simp [Extends]
  | some base =>
    rintro ⟨tail, rfl⟩ ⟨more, rfl⟩
    exact ⟨tail ++ more, by simp [List.append_assoc]⟩

/-- Appends beyond the durable prefix cannot change the tree reached by the old root. -/
theorem reopen_prefix (depth : Nat) (codec : String) (m : Manifest)
    (durable crashed : Option PageFile) (expected : View)
    (kept : Extends durable crashed)
    (read : reopen depth codec ⟨some m, durable⟩ = some expected) :
    reopen depth codec ⟨some m, crashed⟩ = some expected := by
  simp only [reopen] at read ⊢
  split at read
  next checks =>
    rw [if_pos checks]
    cases root : m.tree with
    | none => simpa [readView, root] using read
    | some link =>
      cases durable with
      | none => simp [readView, root] at read
      | some base =>
        obtain ⟨tail, rfl⟩ := kept
        by_cases enough : m.endPos ≤ base.length
        · have enoughAfter : m.endPos ≤ base.length + tail.length := by omega
          simpa [readView, root, enough, enoughAfter,
            List.take_append_of_le_length enough] using read
        · simp [readView, root, enough] at read
  next => contradiction

/-- The staged pages are appended at the actual tail, including prior orphan records. -/
structure Transaction where
  oldRoot : Manifest
  newRoot : Manifest
  /-- Previously durable file base. It need not include the observed orphan tail. -/
  before : Option PageFile
  orphanTail : PageFile
  writes : List DiskPage

def Transaction.synced (txn : Transaction) : PageFile :=
  txn.before.getD [] ++ (txn.orphanTail ++ txn.writes.map some)

/-- New records are written after both the durable prefix and any orphan tail. -/
theorem appended_record_at_tail (txn : Transaction) (index : Nat) :
    txn.synced[(txn.before.getD []).length + txn.orphanTail.length + index]? =
      txn.writes[index]?.map some := by
  simp [Transaction.synced, ← List.append_assoc, List.getElem?_append_right]
  congr 2
  omega

theorem old_prefix_survives_sync (txn : Transaction) :
    Extends txn.before (some txn.synced) := by
  cases prior : txn.before with
  | none => simp [Extends, prior]
  | some base => exact (by
      simp [Extends, Transaction.synced, prior])

inductive Cut where
  | appending
  | pagesSynced
  | pageNameSynced
  | manifestWritten
  | manifestSynced
  | manifestRenamed
  | directorySynced

/-- Crash outcomes expressed as file-system primitives, before interpreting any tree. -/
inductive Crash (txn : Transaction) : Cut → Disk → Prop where
  | appending (file : Option PageFile) (kept : Extends txn.before file) :
      Crash txn .appending ⟨some txn.oldRoot, file⟩
  | pagesSynced (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .pagesSynced ⟨some txn.oldRoot, file⟩
  | pagesNameLost (newFile : txn.before = none) :
      Crash txn .pagesSynced ⟨some txn.oldRoot, none⟩
  | pageNameSynced (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .pageNameSynced ⟨some txn.oldRoot, file⟩
  | manifestWritten (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .manifestWritten ⟨some txn.oldRoot, file⟩
  | manifestSynced (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .manifestSynced ⟨some txn.oldRoot, file⟩
  | renameOld (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .manifestRenamed ⟨some txn.oldRoot, file⟩
  | renameNew (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .manifestRenamed ⟨some txn.newRoot, file⟩
  | directorySynced (file : Option PageFile) (kept : Extends (some txn.synced) file) :
      Crash txn .directorySynced ⟨some txn.newRoot, file⟩

/-- A crash leaves a whole manifest paired with a durable base for that version. -/
theorem Crash.durableVersion (crash : Crash txn cut disk) :
    (disk.manifest = some txn.oldRoot ∧ Extends txn.before disk.pages) ∨
      (disk.manifest = some txn.newRoot ∧ Extends (some txn.synced) disk.pages) := by
  cases crash with
  | appending _ kept => exact Or.inl ⟨rfl, kept⟩
  | pagesNameLost newFile => exact Or.inl ⟨rfl, by simp [Extends, newFile]⟩
  | pagesSynced _ kept | pageNameSynced _ kept | manifestWritten _ kept |
      manifestSynced _ kept | renameOld _ kept =>
    exact Or.inl ⟨rfl, extends_trans (old_prefix_survives_sync txn) kept⟩
  | renameNew _ kept | directorySynced _ kept => exact Or.inr ⟨rfl, kept⟩

/-- The result includes the whole manifest, all reachable pages and row values. -/
theorem crash_reopens_whole_version (depth : Nat) (codec : String) (txn : Transaction)
    (old new : View)
    (oldImage : reopen depth codec ⟨some txn.oldRoot, txn.before⟩ = some old)
    (newImage : reopen depth codec ⟨some txn.newRoot, some txn.synced⟩ = some new)
    (crash : Crash txn cut disk) :
    reopen depth codec disk = some old ∨ reopen depth codec disk = some new := by
  rcases crash.durableVersion with ⟨manifest, kept⟩ | ⟨manifest, kept⟩
  · left
    rcases disk with ⟨m, pages⟩
    change m = some txn.oldRoot at manifest
    subst m
    exact reopen_prefix depth codec txn.oldRoot txn.before pages old kept oldImage
  · right
    rcases disk with ⟨m, pages⟩
    change m = some txn.newRoot at manifest
    subst m
    exact reopen_prefix depth codec txn.newRoot (some txn.synced) pages new kept newImage

theorem confirmed_reopens_new (depth : Nat) (codec : String) (txn : Transaction) (new : View)
    (newImage : reopen depth codec ⟨some txn.newRoot, some txn.synced⟩ = some new)
    (crash : Crash txn .directorySynced disk) : reopen depth codec disk = some new := by
  cases crash with
  | directorySynced file kept =>
    exact reopen_prefix depth codec txn.newRoot (some txn.synced) file new kept newImage

/-- The empty manifest is created and made durable before the first page write. -/
inductive Bootstrap (codec : String) : Disk → Prop where
  | beforeCreate : Bootstrap codec ⟨none, none⟩
  | emptyPublished : Bootstrap codec ⟨some (emptyManifest codec), none⟩

theorem bootstrap_reopens_empty (depth : Nat) (codec : String) (boot : Bootstrap codec disk) :
    reopen depth codec disk = some ⟨emptyManifest codec, none⟩ := by
  cases boot <;> simp [reopen, readView, emptyManifest]

def View.ofRoot (m : Manifest) (root : Root) : View :=
  ⟨m, root.link.map Prod.snd⟩

/-- The publication proof applies to the previous logical update model once its
    old and staged trees are represented by the respective file images. -/
theorem crash_after_update (depth : Nat) (codec : String) (txn : Transaction)
    (edit : Edit) (root next : Root) (valid : root.Exact)
    (updated : updateRoot edit root = some next)
    (oldImage : reopen depth codec ⟨some txn.oldRoot, txn.before⟩ =
      some (View.ofRoot txn.oldRoot root))
    (newImage : reopen depth codec ⟨some txn.newRoot, some txn.synced⟩ =
      some (View.ofRoot txn.newRoot next))
    (crash : Crash txn cut disk) :
    (reopen depth codec disk = some (View.ofRoot txn.oldRoot root) ∨
      reopen depth codec disk = some (View.ofRoot txn.newRoot next)) ∧ next.Exact :=
  ⟨crash_reopens_whole_version depth codec txn _ _ oldImage newImage crash,
    (updateRoot_spec edit root next valid updated).1⟩

-- The name of a new page file must be durable before the new manifest can survive.
example : reopen 1 "json" ⟨some ⟨1, "json", 2, 1, some (1, 0)⟩, none⟩ = none := by
  rfl

-- A new branch shares the old left leaf and replaces only the right leaf.
private def sharedLeaf : DiskPage := .leaf [⟨1, "shared"⟩]
private def oldLeaf : DiskPage := .leaf [⟨2, "old"⟩]
private def newLeaf : DiskPage := .leaf [⟨2, "old"⟩, ⟨3, "new"⟩]
private def sample : Transaction :=
  ⟨⟨1, "json", 3, 3, some (1, 2)⟩, ⟨1, "json", 4, 6, some (1, 5)⟩,
    some [some sharedLeaf, some oldLeaf, some (.branch [(1, 0), (2, 1)])],
    [none], [newLeaf, .branch [(1, 0), (2, 4)]]⟩

example (crash : Crash sample cut disk) :
    (reopen 2 "json" disk).map (fun view => (view.manifest.nextId,
      (view.tree.elim [] (flatten 2)).map Row.id)) = some (3, [1, 2]) ∨
    (reopen 2 "json" disk).map (fun view => (view.manifest.nextId,
      (view.tree.elim [] (flatten 2)).map Row.id)) = some (4, [1, 2, 3]) := by
  have old : reopen 2 "json" ⟨some sample.oldRoot, sample.before⟩ =
      some ⟨sample.oldRoot, some (.branch
        [(1, .leaf [⟨1, "shared"⟩]), (2, .leaf [⟨2, "old"⟩])])⟩ := by rfl
  have new : reopen 2 "json" ⟨some sample.newRoot, some sample.synced⟩ =
      some ⟨sample.newRoot, some (.branch
        [(1, .leaf [⟨1, "shared"⟩]), (2, .leaf [⟨2, "old"⟩, ⟨3, "new"⟩])])⟩ := by rfl
  rcases crash_reopens_whole_version 2 "json" sample _ _ old new crash with h | h
  · left; rw [h]; rfl
  · right; rw [h]; rfl

end PersistCollections.BPlusTree.Cow
