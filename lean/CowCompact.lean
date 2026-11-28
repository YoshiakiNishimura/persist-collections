import CowReuse

namespace PersistCollections.BPlusTree.Cow

/-- Count retained slots before an address, starting at a given slot. -/
def keptCount (keep : Nat → Bool) (start : Nat) : Nat → Nat
  | 0 => 0
  | n + 1 => (if keep start then 1 else 0) + keptCount keep (start + 1) n

def pack (keep : Nat → Bool) (rewrite : DiskPage → DiskPage) (start : Nat) :
    PageFile → PageFile
  | [] => []
  | slot :: tail =>
    let rest := pack keep rewrite (start + 1) tail
    if keep start then slot.map rewrite :: rest else rest

theorem pack_length (keep : Nat → Bool) (rewrite : DiskPage → DiskPage)
    (start : Nat) (file : PageFile) :
    (pack keep rewrite start file).length = keptCount keep start file.length := by
  induction file generalizing start with
  | nil => rfl
  | cons slot tail ih =>
    cases h : keep start <;> simp [pack, keptCount, h, ih, Nat.add_comm]

theorem keptCount_le (keep : Nat → Bool) (start n : Nat) : keptCount keep start n ≤ n := by
  induction n generalizing start with
  | zero => exact Nat.le_refl _
  | succ n ih =>
    have tail := ih (start + 1)
    cases h : keep start <;> simp [keptCount, h] <;> omega

theorem count_monotone (keep : Nat → Bool) (start a b : Nat) (le : a ≤ b) :
    keptCount keep start a ≤ keptCount keep start b := by
  induction a generalizing start b with
  | zero => exact Nat.zero_le _
  | succ a ih =>
    cases b with
    | zero => omega
    | succ b =>
      have step := ih (start + 1) b (by omega)
      simpa only [keptCount] using Nat.add_le_add_left step (if keep start then 1 else 0)

theorem count_strict (keep : Nat → Bool) (start a b : Nat) (lt : a < b)
    (retained : keep (start + a) = true) :
    keptCount keep start a < keptCount keep start b := by
  induction a generalizing start b with
  | zero =>
    cases b with
    | zero => omega
    | succ b =>
      have h : keep start = true := by simpa using retained
      simp [keptCount, h]
      omega
  | succ a ih =>
    cases b with
    | zero => omega
    | succ b =>
      have step := ih (start + 1) b (by omega) (by simpa [Nat.add_assoc, Nat.add_comm 1 a] using retained)
      simpa only [keptCount] using Nat.add_lt_add_left step (if keep start then 1 else 0)

theorem pack_lookup (keep : Nat → Bool) (rewrite : DiskPage → DiskPage)
    (start : Nat) (file : PageFile) (index : Nat) (bound : index < file.length)
    (retained : keep (start + index) = true) :
    (pack keep rewrite start file)[keptCount keep start index]? =
      file[index]?.map (Option.map rewrite) := by
  induction file generalizing start index with
  | nil => simp at bound
  | cons slot rest ih =>
    cases index with
    | zero =>
      have h : keep start = true := by simpa using retained
      simp [pack, keptCount, h]
    | succ index =>
      have tail := ih (start + 1) index (by simpa using bound)
        (by simpa [Nat.add_assoc, Nat.add_comm 1 index] using retained)
      cases h : keep start <;> simp [pack, keptCount, h, Nat.add_comm 1, tail]

def moveLink (move : Address → Address) (link : Link) : Link :=
  (link.1, move link.2)

def movePage (move : Address → Address) : DiskPage → DiskPage
  | .leaf rows => .leaf rows
  | .branch links => .branch (links.map (moveLink move))

private theorem movePage_first (move : Address → Address) (record : DiskPage) :
    (movePage move record).first = record.first := by
  cases record with
  | leaf => rfl
  | branch links => cases links <;> rfl

private theorem mapM_map_list (xs : List α) (f : α → β) (g : β → Option γ) :
    (xs.map f).mapM g = xs.mapM (fun x => g (f x)) := by
  induction xs <;> simp [List.mapM_cons, *]

/-- Relocating records and every child pointer commutes with a successful read. -/
theorem readTree_relocated (before after : PageFile) (move : Address → Address)
    (depth : Nat) (link : Link) (tree : Page)
    (read : readTree before depth link = some tree)
    (copied : ∀ addr ∈ markTree before depth link.2, ∀ record,
      before[addr]?.join = some record → after[move addr]?.join = some (movePage move record))
    (order : ∀ a ∈ markTree before depth link.2, ∀ b ∈ markTree before depth link.2,
      a < b → move a < move b) :
    readTree after depth (moveLink move link) = some tree := by
  induction depth generalizing link tree with
  | zero => contradiction
  | succ depth ih =>
    rcases link with ⟨first, address⟩
    have hm : address ∈ markTree before (depth + 1) address := by simp [markTree]
    cases found : before[address]?.join with
    | none =>
      simp only [readTree] at read
      rw [found] at read
      contradiction
    | some record =>
      have foundAfter := copied address hm record found
      simp only [readTree, found, moveLink, foundAfter] at read ⊢
      simp only [Bind.bind, Option.bind, movePage_first] at read ⊢
      split at read
      next wrong => contradiction
      next correct =>
        rw [if_neg correct]
        cases record with
        | leaf rows => exact read
        | branch links =>
          simp only [movePage] at ⊢
          change (if links.all (fun c => decide (c.2 < address)) then
            (links.mapM (fun c => (readTree before depth c).map (c.1, ·))).map Page.branch
            else none) = some tree at read
          split at read
          next backward =>
            have member (c : Link) (hc : c ∈ links) (a : Address)
                (ha : a ∈ markTree before depth c.2) :
                a ∈ markTree before (depth + 1) address := by
              simp only [markTree, found, List.mem_cons]
              exact Or.inr (List.mem_flatMap.mpr ⟨c, hc, ha⟩)
            let f (c : Link) := (readTree before depth c).map (c.1, ·)
            let g (c : Link) := (readTree after depth (moveLink move c)).map (c.1, ·)
            have done : ∃ ys, links.mapM f = some ys := by
              cases h : links.mapM f with
              | none =>
                change (links.mapM f).map Page.branch = some tree at read
                simp [h] at read
              | some ys => exact ⟨ys, rfl⟩
            have equal : links.mapM g = links.mapM f := by
              apply mapM_agree
              intro c hc
              obtain ⟨pair, ready⟩ := mapM_success_at f links c hc done
              cases h : readTree before depth c with
              | none => simp [f, h] at ready
              | some childTree =>
                have hnew := ih c childTree h
                  (fun a ha => copied a (member c hc a ha))
                  (fun a ha b hb => order a (member c hc a ha) b (member c hc b hb))
                simp [f, g, h, hnew]
            have newBack :
                ((links.map (moveLink move)).all (fun c => decide (c.2 < move address))) = true := by
              rw [List.all_eq_true]
              intro c hc
              obtain ⟨p, hp, rfl⟩ := List.mem_map.mp hc
              have below : p.2 < address := by
                have := (List.all_eq_true.mp backward) p hp
                simpa using this
              have childMarked : p.2 ∈ markTree before (depth + 1) address := by
                cases depth with
                | zero =>
                  obtain ⟨_, ready⟩ := mapM_success_at f links p hp done
                  simp [f, readTree] at ready
                | succ d => exact member p hp p.2 (by simp [markTree])
              simpa [moveLink] using order p.2 childMarked address hm below
            rw [if_pos newBack, mapM_map_list]
            change (links.mapM g).map Page.branch = some tree
            rw [equal]
            exact read
          next => contradiction

def keepLive (depth : Nat) (s : History) (file : PageFile) (addr : Address) : Bool :=
  (liveAddresses depth s file).contains addr

def packedFile (depth : Nat) (s : History) (file : PageFile) : PageFile :=
  let keep := keepLive depth s file
  pack keep (movePage (keptCount keep 0)) 0 file

def movedManifest (depth : Nat) (s : History) (file : PageFile) (m : Manifest) : Manifest :=
  let move := keptCount (keepLive depth s file) 0
  { m with
    endPos := move (min m.endPos file.length)
    tree := m.tree.map (moveLink move) }

def movedView (depth : Nat) (s : History) (file : PageFile) (v : View) : View :=
  { v with manifest := movedManifest depth s file v.manifest }

def View.contents (v : View) : Nat × String × Nat × Option Page :=
  (v.manifest.version, v.manifest.codec, v.manifest.nextId, v.tree)

theorem compact_size (depth : Nat) (s : History) (file : PageFile) :
    (packedFile depth s file).length ≤ file.length := by
  unfold packedFile
  rw [pack_length]
  exact keptCount_le _ _ _

private theorem count_omission (keep : Nat → Bool) (start n index : Nat)
    (inside : index < n) (dead : keep (start + index) = false) :
    keptCount keep start n < n := by
  induction n generalizing start index with
  | zero => omega
  | succ n ih =>
    cases index with
    | zero =>
      have off : keep start = false := by simpa using dead
      have := keptCount_le keep (start + 1) n
      simp [keptCount, off]
      omega
    | succ index =>
      have smaller : index < n := by omega
      have off : keep (start + 1 + index) = false := by
        simpa [Nat.add_assoc, Nat.add_comm 1 index] using dead
      have := ih (start + 1) index smaller off
      cases h : keep start <;> simp [keptCount, h] <;> omega

theorem compact_shrinks (depth : Nat) (s : History) (file : PageFile)
    (addr : Address) (inside : addr < file.length) (dead : addr ∉ liveAddresses depth s file) :
    (packedFile depth s file).length < file.length := by
  unfold packedFile
  rw [pack_length]
  exact count_omission _ 0 file.length addr inside (by simpa [keepLive] using dead)

theorem compact_candidate (depth : Nat) (codec : String) (s : History) (file : PageFile)
    (pages : s.pages = some file) (valid : s.Valid depth codec)
    (v : Generation) (candidate : v ∈ s.candidates) :
    reopen depth codec ⟨some (movedManifest depth s file v.view.manifest),
      some (packedFile depth s file)⟩ = some (movedView depth s file v.view) := by
  let m := v.view.manifest
  let keep := keepLive depth s file
  let move := keptCount keep 0
  let packed := packedFile depth s file
  change reopen depth codec ⟨some (movedManifest depth s file m), some packed⟩ = _
  have read := (valid.2 v candidate).2.2
  rw [pages] at read
  change reopen depth codec ⟨some m, some file⟩ = _ at read
  simp only [reopen] at read ⊢
  split at read
  next checks =>
    cases root : m.tree with
    | none =>
      have result : (⟨m, none⟩ : View) = v.view := by simpa [readView, root] using read
      have checkNew : (movedManifest depth s file m).version = 1 ∧
          (movedManifest depth s file m).codec = codec ∧
          0 < (movedManifest depth s file m).nextId ∧
          (∀ link ∈ (movedManifest depth s file m).tree, 0 < link.1 ∧
            link.1 < (movedManifest depth s file m).nextId ∧
            link.2 < (movedManifest depth s file m).endPos) := by
        simpa [movedManifest, root] using checks
      rw [if_pos checkNew]
      have content := congrArg (movedView depth s file) result
      simpa [readView, movedManifest, movedView, root] using congrArg some content
    | some link =>
      have enough : m.endPos ≤ file.length := by
        by_cases h : m.endPos ≤ file.length
        · exact h
        · simp [readView, root, h] at read
      have hresult : ∃ tree, readTree (file.take m.endPos) depth link = some tree ∧
          (⟨m, some tree⟩ : View) = v.view := by
        simpa [readView, root, enough] using read
      obtain ⟨tree, foundTree, viewEq⟩ := hresult
      have retained (addr : Address) (marked : addr ∈ markTree (file.take m.endPos) depth link.2) :
          keep addr = true := by
        have mark : addr ∈ markManifest depth file m := by simpa [markManifest, root] using marked
        have live : addr ∈ liveAddresses depth s file :=
          List.mem_flatMap.mpr ⟨v, candidate, mark⟩
        simpa [keep, keepLive] using live
      have readNew : readTree (packed.take (move m.endPos)) depth (moveLink move link) = some tree := by
        apply readTree_relocated (file.take m.endPos) _ move depth link tree foundTree
        · intro addr marked record original
          have inner : addr < m.endPos := by
            by_cases h : addr < m.endPos
            · exact h
            · rw [List.getElem?_take_eq_none (Nat.le_of_not_gt h)] at original
              contradiction
          rw [List.getElem?_take_of_lt inner] at original
          have hit := pack_lookup keep (movePage move) 0 file addr (Nat.lt_of_lt_of_le inner enough)
            (by simpa using retained addr marked)
          change packed[move addr]? = _ at hit
          have afterBound : move addr < move m.endPos :=
            count_strict keep 0 addr m.endPos inner (by simpa using retained addr marked)
          rw [List.getElem?_take_of_lt afterBound, hit]
          cases slot : file[addr]? with
          | none => rw [slot] at original; contradiction
          | some opt =>
            cases opt with
            | none => rw [slot] at original; contradiction
            | some p =>
              rw [slot] at original
              cases original
              rfl
        · intro a ha b _ lt
          exact count_strict keep 0 a b lt (by simpa using retained a ha)
      have rootMarked : link.2 ∈ markTree (file.take m.endPos) depth link.2 := by
        cases depth with
        | zero => contradiction
        | succ d => simp [markTree]
      have metadata := checks.2.2.2 link (by simp [root])
      have newBound : move link.2 < move m.endPos := by
        exact count_strict keep 0 link.2 m.endPos metadata.2.2
          (by simpa using retained link.2 rootMarked)
      have endNew : (movedManifest depth s file m).endPos = move m.endPos := by
        simp [movedManifest, keep, move, Nat.min_eq_left enough]
      have enoughNew : move m.endPos ≤ packed.length := by
        change keptCount keep 0 m.endPos ≤ (pack keep (movePage move) 0 file).length
        rw [pack_length]
        exact count_monotone keep 0 m.endPos file.length enough
      have checkNew : (movedManifest depth s file m).version = 1 ∧
          (movedManifest depth s file m).codec = codec ∧
          0 < (movedManifest depth s file m).nextId ∧
          (∀ l ∈ (movedManifest depth s file m).tree, 0 < l.1 ∧
            l.1 < (movedManifest depth s file m).nextId ∧
            l.2 < (movedManifest depth s file m).endPos) := by
        refine ⟨checks.1, checks.2.1, checks.2.2.1, ?_⟩
        intro l hl
        have equal : moveLink move link = l := by simpa [movedManifest, root, keep, move] using hl
        subst l
        exact ⟨metadata.1, metadata.2.1, endNew.symm ▸ newBound⟩
      rw [if_pos checkNew]
      have rootNew : (movedManifest depth s file m).tree = some (moveLink move link) := by
        simp [movedManifest, root, move, keep]
      rw [readView, rootNew, endNew]
      simp [enoughNew, readNew, Bind.bind, Option.bind]
      exact congrArg (movedView depth s file) viewEq
  next => contradiction

def movedGeneration (depth : Nat) (s : History) (file : PageFile) (v : Generation) : Generation :=
  { v with view := movedView depth s file v.view }

def compactHistory (depth : Nat) (s : History) (file : PageFile) : History :=
  let move := movedGeneration depth s file
  { s with
    confirmed := move s.confirmed
    visible := move s.visible
    pending := s.pending.map move
    pages := some (packedFile depth s file) }

theorem compact_history_valid (depth : Nat) (codec : String) (s : History) (file : PageFile)
    (pages : s.pages = some file) (valid : s.Valid depth codec) :
    (compactHistory depth s file).Valid depth codec := by
  have candidates : (compactHistory depth s file).candidates =
      s.candidates.map (movedGeneration depth s file) := rfl
  constructor
  · rw [candidates]
    exact List.mem_map.mpr ⟨s.visible, valid.1, rfl⟩
  · intro v hv
    rw [candidates] at hv
    obtain ⟨source, hc, rfl⟩ := List.mem_map.mp hv
    obtain ⟨floor, freshness, _⟩ := valid.2 source hc
    refine ⟨⟨floor.1, ?_⟩, freshness, compact_candidate depth codec s file pages valid source hc⟩
    intro eq
    exact congrArg (movedView depth s file) (floor.2 eq)

/-- The catalog chooses a complete layout, including the file identity and all
    of the relocated manifests. It is never assembled from two layouts. -/
inductive Layout where
  | old
  | compacted

structure CompactDisk where
  active : Layout
  oldFile : Option PageFile
  newFile : Option PageFile

inductive CompactCut where
  | copying
  | fileSynced
  | catalogSynced
  | renamed
  | directorySynced
  | oldRemoved

/-- Syncing the staged file and its name precedes switching the synced
    catalog. Old-file deletion is enabled only after the switch is durable. -/
inductive CompactCrash (oldFile compacted : PageFile) : CompactCut → CompactDisk → Prop where
  | copying (old new : Option PageFile) (kept : Extends (some oldFile) old) :
      CompactCrash oldFile compacted .copying ⟨.old, old, new⟩
  | fileSynced (old new : Option PageFile) (oldKept : Extends (some oldFile) old) :
      CompactCrash oldFile compacted .fileSynced ⟨.old, old, new⟩
  | catalogSynced (old new : Option PageFile) (oldKept : Extends (some oldFile) old)
      (newKept : Extends (some compacted) new) :
      CompactCrash oldFile compacted .catalogSynced ⟨.old, old, new⟩
  | renameOld (old new : Option PageFile) (oldKept : Extends (some oldFile) old)
      (newKept : Extends (some compacted) new) :
      CompactCrash oldFile compacted .renamed ⟨.old, old, new⟩
  | renameNew (old new : Option PageFile) (oldKept : Extends (some oldFile) old)
      (newKept : Extends (some compacted) new) :
      CompactCrash oldFile compacted .renamed ⟨.compacted, old, new⟩
  | directorySynced (old new : Option PageFile) (oldKept : Extends (some oldFile) old)
      (newKept : Extends (some compacted) new) :
      CompactCrash oldFile compacted .directorySynced ⟨.compacted, old, new⟩
  | oldRemoved (old new : Option PageFile) (newKept : Extends (some compacted) new) :
      CompactCrash oldFile compacted .oldRemoved ⟨.compacted, old, new⟩

def openCandidate (depth : Nat) (codec : String) (s : History) (file : PageFile)
    (disk : CompactDisk) (v : Generation) : Option View :=
  match disk.active with
  | .old => reopen depth codec ⟨some v.view.manifest, disk.oldFile⟩
  | .compacted => reopen depth codec
      ⟨some (movedManifest depth s file v.view.manifest), disk.newFile⟩

/-- In every crash cut, each recovery candidate reopens its exact logical
    contents, including the entire tree and the allocation counter. -/
theorem compact_crash_candidate (depth : Nat) (codec : String)
    (s : History) (file : PageFile) (pages : s.pages = some file) (valid : s.Valid depth codec)
    (crash : CompactCrash file (packedFile depth s file) cut disk)
    (v : Generation) (candidate : v ∈ s.candidates) :
    (openCandidate depth codec s file disk v).map View.contents = some v.view.contents := by
  have oldRead := (valid.2 v candidate).2.2
  rw [pages] at oldRead
  have newRead := compact_candidate depth codec s file pages valid v candidate
  cases crash with
  | copying old _ kept | fileSynced old _ kept |
      catalogSynced old _ kept _ | renameOld old _ kept _ =>
    have read := reopen_prefix depth codec v.view.manifest (some file) old v.view kept oldRead
    simp [openCandidate, read]
  | renameNew _ new _ kept | directorySynced _ new _ kept | oldRemoved _ new kept =>
    have read := reopen_prefix depth codec _ (some (packedFile depth s file)) new
      (movedView depth s file v.view) kept newRead
    simp only [openCandidate, read, Option.map_some]
    rfl

/-- Choosing any surviving generation preserves the confirmed floor and
    returns one complete view from that generation. -/
theorem restart_during_compaction (depth : Nat) (codec : String)
    (s : History) (file : PageFile) (pages : s.pages = some file) (valid : s.Valid depth codec)
    (crash : CompactCrash file (packedFile depth s file) cut disk)
    (v : Generation) (survived : v ∈ s.candidates) :
    v.AtLeast s.confirmed ∧
      (openCandidate depth codec s file disk v).map View.contents = some v.view.contents :=
  ⟨(valid.2 v survived).1, compact_crash_candidate depth codec s file pages valid crash v survived⟩

/-- Cleanup can be retried once the catalog switch is durable. -/
theorem CompactCrash.remove_old
    (crash : CompactCrash oldFile compacted .directorySynced disk) :
    CompactCrash oldFile compacted .oldRemoved { disk with oldFile := none } := by
  cases crash with
  | directorySynced _ new _ kept => exact .oldRemoved none new kept

private def shared : Row := ⟨1, "shared"⟩
private def oldRow : Row := ⟨2, "old"⟩
private def newRow : Row := ⟨3, "new"⟩

private def sampleFile : PageFile :=
  [none, some (.leaf [shared]), some (.leaf [oldRow]), some (.branch [(1, 1), (2, 2)]),
    some (.leaf [⟨9, "orphan"⟩]), some (.leaf [oldRow, newRow]), some (.branch [(1, 1), (2, 5)])]

private def sampleOld : Generation :=
  ⟨1, ⟨⟨1, "json", 3, 4, some (1, 3)⟩, some (.branch
    [(1, .leaf [shared]), (2, .leaf [oldRow])])⟩⟩

private def sampleNew : Generation :=
  ⟨2, ⟨⟨1, "json", 4, 7, some (1, 6)⟩, some (.branch
    [(1, .leaf [shared]), (2, .leaf [oldRow, newRow])])⟩⟩

private def sample : History :=
  ⟨sampleOld, sampleNew, [sampleNew], some sampleFile, 3⟩

private theorem sample_valid : sample.Valid 2 "json" := by
  refine ⟨by simp [sample, History.candidates], ?_⟩
  intro v hv
  have hv : v = sampleOld ∨ v = sampleNew := by simpa [sample, History.candidates] using hv
  rcases hv with rfl | rfl
  · exact ⟨Generation.atLeastSelf _, by decide, rfl⟩
  · refine ⟨⟨by decide, ?_⟩, by decide, rfl⟩
    intro eq
    change 1 = 2 at eq
    contradiction

-- Two slots disappear. Both roots, their end positions, and their shared link move.
example :
    let old := movedManifest 2 sample sampleFile sampleOld.view.manifest
    let new := movedManifest 2 sample sampleFile sampleNew.view.manifest
    ((packedFile 2 sample sampleFile).length, old.tree, old.endPos, new.tree, new.endPos) =
      (5, some (1, 2), 3, some (1, 4), 5) := by decide

example (crash : CompactCrash sampleFile (packedFile 2 sample sampleFile) cut disk) :
    (openCandidate 2 "json" sample sampleFile disk sampleOld).map View.contents =
      some sampleOld.view.contents ∧
    (openCandidate 2 "json" sample sampleFile disk sampleNew).map View.contents =
      some sampleNew.view.contents :=
  ⟨compact_crash_candidate 2 "json" sample sampleFile rfl sample_valid crash sampleOld
      (by simp [sample, History.candidates]),
    compact_crash_candidate 2 "json" sample sampleFile rfl sample_valid crash sampleNew
      (by simp [sample, History.candidates])⟩

end PersistCollections.BPlusTree.Cow
