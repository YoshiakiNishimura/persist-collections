import CowReclaim

namespace PersistCollections.BPlusTree.Cow

/-- Empty slots can be reserved only after checking all recovery roots in the
    same locked state. An out-of-bounds address is not an empty slot. -/
structure Reservation (depth : Nat) (s : History) (file : PageFile)
    (targets : List Address) : Prop where
  currentFile : s.pages = some file
  unique : targets.Nodup
  unused : ∀ addr ∈ targets, file[addr]? = some none ∧ addr ∉ liveAddresses depth s file

theorem Reservation.excludes_reachable
    (reserved : Reservation depth s file targets) (valid : s.Valid depth codec)
    (candidate : v ∈ s.candidates) (root : v.view.manifest.tree = some link)
    (reached : Reachable (file.take v.view.manifest.endPos) link.2 addr) :
    addr ∉ targets := by
  intro chosen
  exact (reserved.unused addr chosen).2
    (candidate_reachable_live depth codec s file reserved.currentFile valid v candidate link root reached)

abbrev SlotWrites := List (Address × DiskPage)

def writeSlots (writes : SlotWrites) (file : PageFile) : PageFile :=
  file.mapIdx (fun addr previous => match writes.lookup addr with
    | some record => some record
    | none => previous)

@[simp] theorem writeSlots_empty (file : PageFile) : writeSlots [] file = file := by
  apply List.ext_getElem?
  intro addr
  simp [writeSlots]

theorem writeSlots_image (writes : SlotWrites) (file : PageFile) :
    ReclaimImage (writes.map Prod.fst) file (writeSlots writes file) := by
  refine ⟨by simp [writeSlots], ?_⟩
  intro addr outside
  have miss : writes.lookup addr = none := by
    apply List.lookup_eq_none_iff.mpr
    intro pair member
    simp only [bne_iff_ne]
    intro eq
    exact outside (List.mem_map.mpr ⟨pair, member, eq.symm⟩)
  simp only [writeSlots, List.getElem?_mapIdx, miss]
  cases file[addr]? <;> rfl

private theorem lookup_reserved (writes : SlotWrites)
    (unique : (writes.map Prod.fst).Nodup) (member : (addr, page) ∈ writes) :
    writes.lookup addr = some page := by
  induction writes with
  | nil => contradiction
  | cons head rest ih =>
    obtain ⟨absent, tail⟩ := List.nodup_cons.mp unique
    rcases List.mem_cons.mp member with eq | later
    · subst head
      simp
    · rcases head with ⟨other, record⟩
      have different : addr ≠ other := by
        intro eq
        exact absent (List.mem_map.mpr ⟨(addr, page), later, eq⟩)
      have test : (addr == other) = false := by
        cases eq : addr == other <;> simp_all
      simp [List.lookup_cons, test, ih tail later]

theorem writeSlots_written (writes : SlotWrites)
    (reserved : Reservation depth s file (writes.map Prod.fst)) (member : (addr, page) ∈ writes) :
    (writeSlots writes file)[addr]? = some (some page) := by
  have address : addr ∈ writes.map Prod.fst := List.mem_map.mpr ⟨(addr, page), member, rfl⟩
  have empty := (reserved.unused addr address).1
  simp [writeSlots, lookup_reserved writes reserved.unique member, empty]

/-- A torn overwrite can change any reserved slot, but the old manifest
    candidates still open their exact former views. -/
theorem Reservation.preserves (reserved : Reservation depth s file targets)
    (outcome : ReclaimImage targets file after) (valid : s.Valid depth codec) :
    ({ s with pages := some after } : History).Valid depth codec := by
  refine ⟨valid.1, ?_⟩
  intro v candidate
  obtain ⟨floor, fresh, read⟩ := valid.2 v candidate
  rw [reserved.currentFile] at read
  rw [reclaim_preserves_candidate depth codec s file after targets
    (fun addr chosen => (reserved.unused addr chosen).2) outcome v candidate] at read
  exact ⟨floor, fresh, read⟩

/-- The existing publication protocol sees the overwritten slots as part of
    its staged file, then appends any fresh records (for example parents). -/
def reuseTransaction (s : History) (file : PageFile) (writes : SlotWrites)
    (append : List DiskPage) (manifest : Manifest) : Transaction :=
  ⟨s.visible.view.manifest, manifest, some (writeSlots writes file), [], append⟩

def pointsBackward (address : Address) : DiskPage → Prop
  | .leaf _ => True
  | .branch links => ∀ link ∈ links, link.2 < address

/-- Placement keeps the existing reader's strictly backward child links. -/
def ReusePlacement (file : PageFile) (writes : SlotWrites) (append : List DiskPage) : Prop :=
  (∀ w ∈ writes, pointsBackward w.1 w.2) ∧
  (∀ w ∈ append.zipIdx, pointsBackward (file.length + w.2) w.1)

/-- A new root can share live pages or use staged records, but cannot use
    bytes from an orphan slot that is absent from the write plan. -/
def ReuseSources (depth : Nat) (s : History) (file : PageFile) (writes : SlotWrites)
    (append : List DiskPage) (manifest : Manifest) : Prop :=
  let synced := (reuseTransaction s file writes append manifest).synced
  ∀ addr ∈ markManifest depth synced manifest,
    addr ∈ liveAddresses depth s file ∨ addr ∈ writes.map Prod.fst ∨
      (file.length ≤ addr ∧ addr < synced.length)

/-- Before the synced image is published, restart can observe a partial
    overwrite only through previously published roots. Successful rename
    adds the newly readable manifest to the protected candidate set. -/
inductive Reuse (depth : Nat) (codec : String) (s : History) : History → Prop where
  | interrupted {file after : PageFile} {targets : List Address}
      (reserved : Reservation depth s file targets)
      (outcome : ReclaimImage targets file after) :
      Reuse depth codec s { s with pages := some after }
  | published {file : PageFile} {writes : SlotWrites}
      (append : List DiskPage) (new : View)
      (reserved : Reservation depth s file (writes.map Prod.fst))
      (placement : ReusePlacement file writes append)
      (sources : ReuseSources depth s file writes append new.manifest)
      (endMatches : new.manifest.endPos =
        (reuseTransaction s file writes append new.manifest).synced.length)
      (ready : reopen depth codec ⟨some new.manifest,
        some (reuseTransaction s file writes append new.manifest).synced⟩ = some new) :
      Reuse depth codec s
        (s.afterRename (reuseTransaction s file writes append new.manifest) new)

theorem Reuse.preserves (reuse : Reuse depth codec s next)
    (valid : s.Valid depth codec) :
    next.Valid depth codec ∧ next.confirmed = s.confirmed := by
  cases reuse with
  | interrupted reserved outcome => exact ⟨reserved.preserves outcome valid, rfl⟩
  | @published file writes append new reserved placement sources endMatches ready =>
    have staged := reserved.preserves (writeSlots_image writes file) valid
    let base := { s with pages := some (writeSlots writes file) }
    let txn := reuseTransaction s file writes append new.manifest
    have rename : Step depth codec base (s.afterRename txn new) :=
      .rename base txn new rfl rfl rfl ready
    exact ⟨(rename.preserves staged).1, rfl⟩

/-- Before publication a crash can only reopen a prior candidate, even if
    some reserved slots now contain complete replacements and others do not. -/
theorem restart_during_overwrite (depth : Nat) (codec : String)
    (reserved : Reservation depth s file targets) (outcome : ReclaimImage targets file after)
    (valid : s.Valid depth codec)
    (restart : Restart { s with pages := some after } kind disk next) :
    ∃ v ∈ s.candidates, v.AtLeast s.confirmed ∧
      next.visible = v ∧ reopen depth codec disk = some v.view := by
  obtain ⟨v, member, floor, _, visible, read⟩ :=
    restart_after_trace depth codec (Trace.nil _) (reserved.preserves outcome valid) restart
  exact ⟨v, member, floor, visible, read⟩

inductive ReuseTrace (depth : Nat) (codec : String) : History → History → Prop where
  | nil (s : History) : ReuseTrace depth codec s s
  | ordinary {start middle next : History} : ReuseTrace depth codec start middle →
      ReclaimTrace depth codec middle next → ReuseTrace depth codec start next
  | reuse {start middle next : History} : ReuseTrace depth codec start middle →
      Reuse depth codec middle next → ReuseTrace depth codec start next

theorem ReuseTrace.preserves (trace : ReuseTrace depth codec start next)
    (valid : start.Valid depth codec) :
    next.Valid depth codec ∧ next.confirmed.AtLeast start.confirmed := by
  induction trace with
  | nil => exact ⟨valid, Generation.atLeastSelf _⟩
  | ordinary _ ordinary ih =>
    obtain ⟨nextValid, floor⟩ := ordinary.preserves ih.1
    exact ⟨nextValid, Generation.atLeastTrans floor ih.2⟩
  | reuse _ reuse ih =>
    obtain ⟨nextValid, sameFloor⟩ := reuse.preserves ih.1
    exact ⟨nextValid, sameFloor.symm ▸ ih.2⟩

/-- Repeated reservations, overwrites, publishes, and reboots preserve one
    complete view at or above the confirmed floor. -/
theorem restart_after_reuse_trace (depth : Nat) (codec : String)
    (trace : ReuseTrace depth codec start s) (valid : start.Valid depth codec)
    (restart : Restart s kind disk next) :
    ∃ v ∈ s.candidates, v.AtLeast s.confirmed ∧ v.AtLeast start.confirmed ∧
      next.visible = v ∧ reopen depth codec disk = some v.view := by
  obtain ⟨current, floor⟩ := trace.preserves valid
  obtain ⟨v, member, recent, _, visible, read⟩ :=
    restart_after_trace depth codec (Trace.nil s) current restart
  exact ⟨v, member, recent, Generation.atLeastTrans recent floor, visible, read⟩

private def firstRow : Row := ⟨1, "shared"⟩
private def secondRow : Row := ⟨2, "old"⟩
private def addedRow : Row := ⟨3, "new"⟩

private def old : View :=
  ⟨⟨1, "json", 3, 4, some (1, 3)⟩, some (.branch
    [(1, .leaf [firstRow]), (2, .leaf [secondRow])])⟩

private def sampleFile : PageFile :=
  [none, some (.leaf [firstRow]), some (.leaf [secondRow]), some (.branch [(1, 1), (2, 2)])]

private def sample : History :=
  ⟨⟨1, old⟩, ⟨1, old⟩, [], some sampleFile, 2⟩

private def sampleWrites : SlotWrites := [(0, .leaf [addedRow])]
private def newParent : List DiskPage := [.branch [(1, 1), (2, 2), (3, 0)]]

private def newView : View :=
  ⟨⟨1, "json", 4, 5, some (1, 4)⟩, some (.branch
    [(1, .leaf [firstRow]), (2, .leaf [secondRow]), (3, .leaf [addedRow])])⟩

private def afterPublish : History :=
  sample.afterRename (reuseTransaction sample sampleFile sampleWrites newParent newView.manifest) newView

private theorem sampleReserved : Reservation 2 sample sampleFile [0] := by
  refine ⟨rfl, by decide, ?_⟩
  intro addr chosen
  have eq : addr = 0 := by simpa using chosen
  subst addr
  exact ⟨rfl, by decide⟩

private theorem sampleValid : sample.Valid 2 "json" := by
  refine ⟨by simp [sample, History.candidates], ?_⟩
  intro v member
  have eq : v = sample.confirmed := by simpa [sample, History.candidates] using member
  subst v
  exact ⟨Generation.atLeastSelf _, by decide, rfl⟩

-- Reusing a low slot for a leaf works when its parent is appended.
private theorem samplePublish : Reuse 2 "json" sample afterPublish := by
  apply Reuse.published (file := sampleFile) (writes := sampleWrites) newParent newView sampleReserved
  · simp [ReusePlacement, sampleFile, sampleWrites, newParent, pointsBackward]
    intro _ childAddress child
    rcases child with ⟨_, eq⟩ | ⟨_, eq⟩ | ⟨_, eq⟩ <;> subst childAddress <;> decide
  · unfold ReuseSources
    decide
  · rfl
  · rfl

-- That same low slot cannot hold a parent with a higher-address child.
example : ¬ pointsBackward 0 (.branch [(1, 1)]) := by simp [pointsBackward]

-- Even a malformed replacement at the reused address leaves the old view readable.
example :
    let damaged := writeSlots [(0, .branch [(3, 0)])] sampleFile
    ({ sample with pages := some damaged } : History).Valid 2 "json" :=
  sampleReserved.preserves (writeSlots_image [(0, .branch [(3, 0)])] sampleFile) sampleValid

-- A successful rename protects the former hole while the new view is still pending.
example : ∃ file, afterPublish.pages = some file ∧ 0 ∈ liveAddresses 2 afterPublish file := by
  refine ⟨_, rfl, ?_⟩
  decide

-- Recovery from the pending publication can still select the old complete view.
example :
    let disk : Disk := ⟨some old.manifest, afterPublish.pages⟩
    reopen 2 "json" disk = some old := by
  have valid := (samplePublish.preserves sampleValid).1
  exact (valid.2 afterPublish.confirmed (by simp [History.candidates])).2.2

end PersistCollections.BPlusTree.Cow
