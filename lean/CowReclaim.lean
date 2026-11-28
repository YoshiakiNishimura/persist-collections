import CowHistory

namespace PersistCollections.BPlusTree.Cow

/-- Mark every slot the bounded reader can visit, including shared children.
    Following even rejected branch records is conservative. -/
def markTree (file : PageFile) : Nat → Address → List Address
  | 0, _ => []
  | depth + 1, address =>
    address :: match file[address]?.join with
      | some (.branch links) => links.flatMap (fun child => markTree file depth child.2)
      | _ => []

def markManifest (depth : Nat) (file : PageFile) (m : Manifest) : List Address :=
  m.tree.elim [] (fun link => markTree (file.take m.endPos) depth link.2)

def liveAddresses (depth : Nat) (s : History) (file : PageFile) : List Address :=
  s.candidates.flatMap (fun v => markManifest depth file v.view.manifest)

/-- Physical reachability is independent of the reader's depth budget. -/
inductive Reachable (file : PageFile) : Address → Address → Prop where
  | root (address : Address) : Reachable file address address
  | via (links : List Link) (child : Link)
      (decoded : file[parent]?.join = some (.branch links)) (member : child ∈ links)
      (below : Reachable file child.2 address) : Reachable file parent address

private theorem mapM_success_at (f : α → Option β) (xs : List α) (x : α)
    (member : x ∈ xs) (done : ∃ ys, xs.mapM f = some ys) : ∃ y, f x = some y := by
  induction xs with
  | nil => simp at member
  | cons a xs ih =>
    obtain ⟨ys, done⟩ := done
    cases fa : f a with
    | none => simp [List.mapM_cons, fa, Bind.bind, Option.bind] at done
    | some y =>
      rcases List.mem_cons.mp member with eq | tail
      · subst x
        exact ⟨y, fa⟩
      · apply ih tail
        cases result : xs.mapM f with
        | none => simp [List.mapM_cons, fa, result, Bind.bind, Option.bind] at done
        | some rest => exact ⟨rest, rfl⟩

/-- On success, the bounded traversal marks all addresses reachable through
    page links, regardless of how many paths share a page. -/
theorem reachable_marked (file : PageFile) (depth : Nat) (link : Link) (tree : Page)
    (read : readTree file depth link = some tree) (reached : Reachable file link.2 addr) :
    addr ∈ markTree file depth link.2 := by
  induction depth generalizing link tree with
  | zero => contradiction
  | succ depth ih =>
    rcases link with ⟨first, address⟩
    cases reached with
    | root => simp [markTree]
    | via links child decoded member below =>
      let descend (c : Link) := (readTree file depth c).map (c.1, ·)
      have success : ∃ children, links.mapM descend = some children := by
        cases result : links.mapM descend with
        | none =>
          simp only [readTree] at read
          change file[address]?.join = _ at decoded
          rw [decoded] at read
          simp [Bind.bind, Option.bind, show
            links.mapM (fun c => (readTree file depth c).map (c.1, ·)) = none from result] at read
        | some children => exact ⟨children, rfl⟩
      obtain ⟨_, visited⟩ := mapM_success_at descend links child member success
      cases h : readTree file depth child with
      | none => simp [descend, h] at visited
      | some subtree =>
        have marked := ih child subtree h below
        simp only [markTree, decoded, List.mem_cons]
        exact Or.inr (List.mem_flatMap.mpr ⟨child, member, marked⟩)

private theorem mapM_agree (f g : α → Option β) (xs : List α)
    (same : ∀ x ∈ xs, f x = g x) : xs.mapM f = xs.mapM g := by
  induction xs with
  | nil => rfl
  | cons x xs ih =>
    simp only [List.mapM_cons]
    rw [same x (by simp), ih (fun y hy => same y (List.mem_cons_of_mem x hy))]

/-- Equal records at the marked addresses suffice to reproduce the exact tree. -/
theorem readTree_agrees (before after : PageFile) (depth : Nat) (link : Link)
    (same : ∀ addr ∈ markTree before depth link.2, before[addr]? = after[addr]?) :
    readTree before depth link = readTree after depth link := by
  induction depth generalizing link with
  | zero => rfl
  | succ depth ih =>
    rcases link with ⟨first, address⟩
    have rootEq := same address (by simp [markTree])
    simp only [readTree, ← rootEq]
    cases decoded : before[address]?.join with
    | none => rfl
    | some page =>
      cases page with
      | leaf rows => rfl
      | branch links =>
        have children :
            links.mapM (fun c => (readTree before depth c).map (c.1, ·)) =
              links.mapM (fun c => (readTree after depth c).map (c.1, ·)) := by
          apply mapM_agree
          intro c hc
          rw [ih c]
          intro addr marked
          apply same
          simp only [markTree, decoded, List.mem_cons]
          exact Or.inr (List.mem_flatMap.mpr ⟨c, hc, marked⟩)
        simp [Bind.bind, Option.bind, children]

/-- The manifest and every page of its view are stable under changes elsewhere. -/
theorem reopen_agrees (depth : Nat) (codec : String) (m : Manifest) (before after : PageFile)
    (size : after.length = before.length)
    (same : ∀ addr ∈ markManifest depth before m, before[addr]? = after[addr]?) :
    reopen depth codec ⟨some m, some before⟩ = reopen depth codec ⟨some m, some after⟩ := by
  simp only [reopen]
  split
  next =>
    cases root : m.tree with
    | none => simp [readView, root]
    | some link =>
      have eq : readTree (before.take m.endPos) depth link =
          readTree (after.take m.endPos) depth link := by
        apply readTree_agrees
        intro addr marked
        have hm : addr ∈ markManifest depth before m := by
          simpa [markManifest, root] using marked
        rw [List.getElem?_take, List.getElem?_take, same addr hm]
      simp [readView, root, size, eq]
  next => rfl

/-- Choose occupied slots outside the union of all recovery candidates' marks. -/
def reclaimable (depth : Nat) (s : History) (file : PageFile) : List Address :=
  (List.range file.length).filter (fun addr =>
    !(liveAddresses depth s file).contains addr && file[addr]?.join.isSome)

theorem reclaimable_unmarked (depth : Nat) (s : History) (file : PageFile)
    (addr : Address) (chosen : addr ∈ reclaimable depth s file) :
    addr ∉ liveAddresses depth s file := by
  have marked := (List.mem_filter.mp chosen).2
  simp only [Bool.and_eq_true, Bool.not_eq_true', List.contains_eq_mem] at marked
  simpa using marked.1

theorem reclaimable_excludes_reachable (depth : Nat) (codec : String) (s : History)
    (file : PageFile) (pages : s.pages = some file) (valid : s.Valid depth codec)
    (v : Generation) (candidate : v ∈ s.candidates) (link : Link)
    (root : v.view.manifest.tree = some link)
    (reached : Reachable (file.take v.view.manifest.endPos) link.2 addr) :
    addr ∉ reclaimable depth s file := by
  have read := (valid.2 v candidate).2.2
  rw [pages] at read
  obtain ⟨tree, found⟩ : ∃ tree, readTree (file.take v.view.manifest.endPos) depth link = some tree := by
    cases found : readTree (file.take v.view.manifest.endPos) depth link with
    | none => simp [reopen, readView, root, found] at read
    | some tree => exact ⟨tree, rfl⟩
  have marked : addr ∈ markManifest depth file v.view.manifest := by
    simpa [markManifest, root] using
      reachable_marked (file.take v.view.manifest.endPos) depth link tree found reached
  intro chosen
  exact reclaimable_unmarked depth s file addr chosen
    (List.mem_flatMap.mpr ⟨v, candidate, marked⟩)

/-- A completed reclamation leaves holes and retains physical slot numbers. -/
def clearSlots (targets : List Address) (file : PageFile) : PageFile :=
  file.mapIdx (fun addr page => if addr ∈ targets then none else page)

/-- An interrupted reclaim may leave any contents in targeted slots.
    The operation preserves the file name, length, and all other slots. -/
structure ReclaimImage (targets : List Address) (before after : PageFile) : Prop where
  sameLength : after.length = before.length
  outside : ∀ addr ∉ targets, before[addr]? = after[addr]?

theorem clearSlots_image (targets : List Address) (file : PageFile) :
    ReclaimImage targets file (clearSlots targets file) := by
  refine ⟨by simp [clearSlots], ?_⟩
  intro addr outside
  simp only [clearSlots, List.getElem?_mapIdx, if_neg outside]
  cases file[addr]? <;> rfl

/-- A partial reclaim preserves each candidate's complete image, not just the visible root. -/
theorem reclaim_preserves_candidate (depth : Nat) (codec : String) (s : History)
    (before after : PageFile) (targets : List Address)
    (safe : ∀ addr ∈ targets, addr ∉ liveAddresses depth s before)
    (outcome : ReclaimImage targets before after)
    (v : Generation) (candidate : v ∈ s.candidates) :
    reopen depth codec ⟨some v.view.manifest, some before⟩ =
      reopen depth codec ⟨some v.view.manifest, some after⟩ := by
  apply reopen_agrees depth codec v.view.manifest before after outcome.sameLength
  intro addr marked
  apply outcome.outside
  intro targeted
  exact safe addr targeted (List.mem_flatMap.mpr ⟨v, candidate, marked⟩)

/-- Planning and applying the reclaim share one locked state. Outcomes include
    every crash cut, even when only part of a planned hole is made. -/
inductive Reclaim (depth : Nat) (s : History) : History → Prop where
  | apply (before after : PageFile) (targets : List Address)
      (pages : s.pages = some before)
      (safe : ∀ addr ∈ targets, addr < before.length ∧ addr ∉ liveAddresses depth s before)
      (outcome : ReclaimImage targets before after) :
      Reclaim depth s { s with pages := some after }

theorem reclaim_all (depth : Nat) (s : History) (file : PageFile) (pages : s.pages = some file) :
    Reclaim depth s { s with pages := some (clearSlots (reclaimable depth s file) file) } := by
  apply Reclaim.apply _ _ _ pages _ (clearSlots_image _ _)
  intro addr chosen
  exact ⟨List.mem_range.mp (List.mem_filter.mp chosen).1,
    reclaimable_unmarked depth s file addr chosen⟩

theorem Reclaim.preserves {depth : Nat} {codec : String} {s next : History}
    (step : Reclaim depth s next) (valid : s.Valid depth codec) :
    next.Valid depth codec ∧ next.confirmed = s.confirmed := by
  cases step with
  | apply before after targets pages safe outcome =>
    refine ⟨⟨valid.1, ?_⟩, rfl⟩
    intro v candidate
    obtain ⟨floor, fresh, read⟩ := valid.2 v candidate
    rw [pages] at read
    rw [reclaim_preserves_candidate depth codec s before after targets
      (fun addr chosen => (safe addr chosen).2) outcome v candidate] at read
    exact ⟨floor, fresh, read⟩

inductive ReclaimTrace (depth : Nat) (codec : String) : History → History → Prop where
  | nil (s : History) : ReclaimTrace depth codec s s
  | ordinary {start middle next : History} : ReclaimTrace depth codec start middle →
      Step depth codec middle next → ReclaimTrace depth codec start next
  | reclaim {start middle next : History} : ReclaimTrace depth codec start middle →
      Reclaim depth middle next → ReclaimTrace depth codec start next

theorem ReclaimTrace.preserves {depth : Nat} {codec : String} {start next : History}
    (trace : ReclaimTrace depth codec start next) (valid : start.Valid depth codec) :
    next.Valid depth codec ∧ next.confirmed.AtLeast start.confirmed := by
  induction trace with
  | nil => exact ⟨valid, Generation.atLeastSelf _⟩
  | ordinary _ ordinary ih =>
    obtain ⟨midValid, midFloor⟩ := ih
    obtain ⟨nextValid, floor⟩ := ordinary.preserves midValid
    exact ⟨nextValid, Generation.atLeastTrans floor midFloor⟩
  | reclaim _ reclaim ih =>
    obtain ⟨midValid, midFloor⟩ := ih
    obtain ⟨nextValid, sameFloor⟩ := reclaim.preserves midValid
    exact ⟨nextValid, sameFloor.symm ▸ midFloor⟩

/-- Even a crash during reclamation yields one complete candidate, at or
    above the last confirmed view, after an arbitrary interleaving. -/
theorem restart_after_reclaim_trace (depth : Nat) (codec : String)
    (trace : ReclaimTrace depth codec start s) (valid : start.Valid depth codec)
    (restart : Restart s kind disk next) :
    ∃ v ∈ s.candidates, v.AtLeast s.confirmed ∧ v.AtLeast start.confirmed ∧
      next.visible = v ∧ reopen depth codec disk = some v.view := by
  obtain ⟨current, floor⟩ := trace.preserves valid
  obtain ⟨v, member, recent, _, visible, read⟩ :=
    restart_after_trace depth codec (Trace.nil s) current restart
  exact ⟨v, member, recent, Generation.atLeastTrans recent floor, visible, read⟩

private def sharedRow : Row := ⟨1, "shared"⟩
private def oldRow : Row := ⟨2, "old"⟩
private def newRow : Row := ⟨3, "new"⟩

private def oldVersion : Generation :=
  ⟨1, ⟨⟨1, "json", 3, 3, some (1, 2)⟩, some (.branch
    [(1, .leaf [sharedRow]), (2, .leaf [oldRow])])⟩⟩

private def unknownVersion : Generation :=
  ⟨2, ⟨⟨1, "json", 4, 5, some (1, 4)⟩, some (.branch
    [(1, .leaf [sharedRow]), (2, .leaf [oldRow, newRow])])⟩⟩

private def sampleFile : PageFile :=
  [some (.leaf [sharedRow]), some (.leaf [oldRow]), some (.branch [(1, 0), (2, 1)]),
    some (.leaf [oldRow, newRow]), some (.branch [(1, 0), (2, 3)]),
    some (.leaf [⟨8, "orphan"⟩]), some (.leaf [⟨9, "orphan"⟩])]

private def sampleHistory : History :=
  ⟨oldVersion, unknownVersion, [unknownVersion], some sampleFile, 3⟩

-- Both branches and their shared leaf are live until the newer root is confirmed.
example : reclaimable 2 sampleHistory sampleFile = [5, 6] := by decide
example : reclaimable 2 sampleHistory.afterConfirm sampleFile = [1, 2, 5, 6] := by decide

-- A crash interrupts a two-slot reclaim after clearing only the first slot.
-- The old confirmed root can still reopen with its exact former contents.
example :
    let after := clearSlots [5] sampleFile
    reopen 2 "json" ⟨some oldVersion.view.manifest, some after⟩ = some oldVersion.view := by
  dsimp
  have interrupted : ReclaimImage [5, 6] sampleFile (clearSlots [5] sampleFile) := by
    have one := clearSlots_image [5] sampleFile
    refine ⟨one.sameLength, fun addr outside => one.outside addr ?_⟩
    intro member
    have eq : addr = 5 := by simpa using member
    exact outside (by simp [eq])
  have safe : ∀ addr ∈ ([5, 6] : List Address), addr ∉ liveAddresses 2 sampleHistory sampleFile := by
    intro addr chosen
    have plan : reclaimable 2 sampleHistory sampleFile = [5, 6] := by decide
    exact reclaimable_unmarked 2 sampleHistory sampleFile addr (plan.symm ▸ chosen)
  have same := reclaim_preserves_candidate 2 "json" sampleHistory _ _ _ safe interrupted oldVersion
    (by simp [sampleHistory, History.candidates])
  rw [← same]
  rfl

end PersistCollections.BPlusTree.Cow
