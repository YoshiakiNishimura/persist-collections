import CowRecovery

namespace PersistCollections.BPlusTree.Cow

/-- A proof-only generation identifies a publication attempt, even if a
    crash causes the application to reuse an ID or publish the same data. -/
structure Generation where
  number : Nat
  view : View

/-- Equal generations denote the same complete view. -/
def Generation.AtLeast (later earlier : Generation) : Prop :=
  earlier.number ≤ later.number ∧ (earlier.number = later.number → later.view = earlier.view)

theorem Generation.atLeastSelf (v : Generation) : v.AtLeast v :=
  ⟨Nat.le_refl _, fun _ => rfl⟩

theorem Generation.atLeastTrans {a b c : Generation}
    (ab : a.AtLeast b) (bc : b.AtLeast c) : a.AtLeast c := by
  refine ⟨Nat.le_trans bc.1 ab.1, ?_⟩
  intro eq
  have cb : c.number = b.number := by have := bc.1; have := ab.1; omega
  exact (ab.2 (cb.symm.trans eq)).trans (bc.2 cb)

/-- The confirmed floor and the currently visible manifest can differ. -/
structure History where
  confirmed : Generation
  visible : Generation
  pending : List Generation
  pages : Option PageFile
  nextGeneration : Nat

def History.candidates (s : History) : List Generation :=
  s.confirmed :: s.pending

def initial (codec : String) : History :=
  let empty : Generation := ⟨0, ⟨emptyManifest codec, none⟩⟩
  ⟨empty, empty, [], none, 1⟩

/-- Every possible manifest has a complete image in the same durable page
    file and is at or above the acknowledged floor. -/
def History.Valid (depth : Nat) (codec : String) (s : History) : Prop :=
  s.visible ∈ s.candidates ∧
  ∀ v ∈ s.candidates, v.AtLeast s.confirmed ∧ v.number < s.nextGeneration ∧
    reopen depth codec ⟨some v.view.manifest, s.pages⟩ = some v.view

theorem initial_valid (depth : Nat) (codec : String) : (initial codec).Valid depth codec := by
  refine ⟨by simp [initial, History.candidates], ?_⟩
  intro v member
  have eq : v = (initial codec).confirmed := by simpa [initial, History.candidates] using member
  subst v
  refine ⟨Generation.atLeastSelf _, by simp [initial], ?_⟩
  simp [initial, reopen, readView, emptyManifest]

inductive RestartKind where
  | process
  | power

def History.afterPower (s : History) (survivor : Generation) : History :=
  { s with visible := survivor }

/-- Process restart reads the currently visible name. Power recovery may
    retain any whole name since the last durable directory checkpoint.
    Seeing one of those names after reboot is no new directory confirmation. -/
inductive Restart (s : History) : RestartKind → Disk → History → Prop where
  | process (file : Option PageFile) (kept : Extends s.pages file) :
      Restart s .process ⟨some s.visible.view.manifest, file⟩ s
  | power (survivor : Generation) (file : Option PageFile)
      (possible : survivor ∈ s.candidates) (kept : Extends s.pages file) :
      Restart s .power ⟨some survivor.view.manifest, file⟩ (s.afterPower survivor)

def History.afterRename (s : History) (txn : Transaction) (new : View) : History :=
  { s with
    visible := ⟨s.nextGeneration, new⟩
    pending := s.pending ++ [⟨s.nextGeneration, new⟩]
    pages := some txn.synced
    nextGeneration := s.nextGeneration + 1 }

def History.afterConfirm (s : History) : History :=
  { s with confirmed := s.visible, pending := [] }

/-- Rename follows page sync (including the name for a new file) and syncing
    the temporary manifest. A successful final directory sync advances the floor. -/
inductive Step (depth : Nat) (codec : String) : History → History → Prop where
  | pagesOnly (s : History) (pages : Option PageFile) (kept : Extends s.pages pages) :
      Step depth codec s { s with pages := pages }
  | rename (s : History) (txn : Transaction) (new : View)
      (before : txn.before = s.pages)
      (parent : txn.oldRoot = s.visible.view.manifest)
      (newManifest : txn.newRoot = new.manifest)
      (ready : reopen depth codec ⟨some txn.newRoot, some txn.synced⟩ = some new) :
      Step depth codec s (s.afterRename txn new)
  | confirm (s : History) : Step depth codec s s.afterConfirm
  | restart (s next : History) (kind : RestartKind) (disk : Disk)
      (restarted : Restart s kind disk next) : Step depth codec s next

private theorem rename_candidates (s : History) (txn : Transaction) (new : View) :
    (s.afterRename txn new).candidates = s.candidates ++ [⟨s.nextGeneration, new⟩] := rfl

theorem Step.preserves {depth : Nat} {codec : String} {s next : History}
    (step : Step depth codec s next) (valid : s.Valid depth codec) :
    next.Valid depth codec ∧ next.confirmed.AtLeast s.confirmed := by
  cases step with
  | pagesOnly pages kept =>
    refine ⟨⟨valid.1, ?_⟩, Generation.atLeastSelf _⟩
    intro v member
    obtain ⟨low, high, read⟩ := valid.2 v member
    exact ⟨low, high, reopen_prefix depth codec v.view.manifest s.pages pages v.view kept read⟩
  | rename txn new before parent newManifest ready =>
    refine ⟨⟨?_, ?_⟩, Generation.atLeastSelf _⟩
    · rw [rename_candidates]
      exact List.mem_append.mpr (Or.inr (by simp [History.afterRename]))
    · have kept : Extends s.pages (some txn.synced) := before ▸ old_prefix_survives_sync txn
      intro v member
      rw [rename_candidates] at member
      rcases List.mem_append.mp member with old | fresh
      · obtain ⟨low, high, read⟩ := valid.2 v old
        exact ⟨low, Nat.lt_trans high (Nat.lt_succ_self _),
          reopen_prefix depth codec v.view.manifest s.pages (some txn.synced) v.view kept read⟩
      · have eq := List.mem_singleton.mp fresh
        subst v
        obtain ⟨_, high, _⟩ := valid.2 s.confirmed (by simp [History.candidates])
        refine ⟨⟨Nat.le_of_lt high, fun eq => False.elim (Nat.ne_of_lt high eq)⟩,
          Nat.lt_succ_self _, ?_⟩
        simpa only [History.afterRename, ← newManifest] using ready
  | confirm =>
    obtain ⟨floor, high, read⟩ := valid.2 s.visible valid.1
    refine ⟨⟨by simp [History.afterConfirm, History.candidates], ?_⟩, floor⟩
    intro v member
    have eq : v = s.visible := by
      simpa [History.afterConfirm, History.candidates] using member
    subst v
    exact ⟨Generation.atLeastSelf _, high, read⟩
  | restart next kind disk restarted =>
    cases restarted with
    | process => exact ⟨valid, Generation.atLeastSelf _⟩
    | power survivor file member kept => exact ⟨⟨member, valid.2⟩, Generation.atLeastSelf _⟩

inductive Trace (depth : Nat) (codec : String) : History → History → Prop where
  | nil (s : History) : Trace depth codec s s
  | snoc {start middle last : History} : Trace depth codec start middle →
      Step depth codec middle last → Trace depth codec start last

theorem Trace.preserves {depth : Nat} {codec : String} {start finish : History}
    (trace : Trace depth codec start finish) (valid : start.Valid depth codec) :
    finish.Valid depth codec ∧ finish.confirmed.AtLeast start.confirmed := by
  induction trace with
  | nil => exact ⟨valid, Generation.atLeastSelf _⟩
  | snoc _ step ih =>
    obtain ⟨midValid, midFloor⟩ := ih
    obtain ⟨endValid, endFloor⟩ := step.preserves midValid
    exact ⟨endValid, Generation.atLeastTrans endFloor midFloor⟩

/-- After any number of updates and restarts, the next restart reads one
    complete candidate, at least as recent as every previously confirmed view.
    At the same generation, it reads exactly the confirmed view. -/
theorem restart_after_trace (depth : Nat) (codec : String)
    (trace : Trace depth codec start s) (valid : start.Valid depth codec)
    (restart : Restart s kind disk next) :
    ∃ v ∈ s.candidates,
      v.AtLeast s.confirmed ∧ v.AtLeast start.confirmed ∧
      next.visible = v ∧ reopen depth codec disk = some v.view := by
  obtain ⟨current, floor⟩ := trace.preserves valid
  cases restart with
  | process file kept =>
    obtain ⟨low, _, read⟩ := current.2 s.visible current.1
    exact ⟨s.visible, current.1, low, Generation.atLeastTrans low floor, rfl,
      reopen_prefix depth codec _ _ _ _ kept read⟩
  | power v file member kept =>
    obtain ⟨low, _, read⟩ := current.2 v member
    exact ⟨v, member, low, Generation.atLeastTrans low floor, rfl,
      reopen_prefix depth codec _ _ _ _ kept read⟩

theorem restart_from_empty (depth : Nat) (codec : String)
    (trace : Trace depth codec (initial codec) s) (restart : Restart s kind disk next) :
    next.visible.AtLeast s.confirmed ∧ reopen depth codec disk = some next.visible.view := by
  obtain ⟨v, _, floor, _, rfl, read⟩ := restart_after_trace depth codec trace
    (initial_valid depth codec) restart
  exact ⟨floor, read⟩

private def exampleRows (count : Nat) : List Row :=
  (List.range count).map (fun n => ⟨n + 1, ""⟩)

private def exampleTxn (s : History) (count : Nat) : Transaction :=
  let tail := (s.pages.getD []).length
  ⟨s.visible.view.manifest, ⟨1, "json", count + 1, tail + 1, some (1, tail)⟩,
    s.pages, [], [.leaf (exampleRows count)]⟩

private def exampleView (txn : Transaction) (count : Nat) : View :=
  ⟨txn.newRoot, some (.leaf (exampleRows count))⟩

private def exampleCommit (s : History) (count : Nat) : History :=
  s.afterRename (exampleTxn s count) (exampleView (exampleTxn s count) count)

private def one := (exampleCommit (initial "json") 1).afterConfirm
private def two := exampleCommit one 2
private def three := exampleCommit two 3

private theorem exampleTrace : Trace 1 "json" (initial "json") three := by
  have first : Step 1 "json" (initial "json") (exampleCommit (initial "json") 1) :=
    .rename _ _ _ rfl rfl rfl rfl
  have confirmed : Step 1 "json" (exampleCommit (initial "json") 1) one := .confirm _
  have second : Step 1 "json" one two := .rename _ _ _ rfl rfl rfl rfl
  have process : Step 1 "json" two two :=
    .restart _ _ .process _ (.process two.pages (by exact ⟨[], rfl⟩))
  have third : Step 1 "json" two three := .rename _ _ _ rfl rfl rfl rfl
  exact ((((Trace.nil _).snoc first).snoc confirmed).snoc second).snoc process |>.snoc third

-- After a process restart and another uncertain rename, power recovery may
-- find the intermediate version. Its rows and allocation counter agree.
example :
    let disk : Disk := ⟨some two.visible.view.manifest, three.pages⟩
    (reopen 1 "json" disk).map (fun v =>
      (v.manifest.nextId, (v.tree.elim [] (flatten 1)).map Row.id)) = some (3, [1, 2]) := by
  have crash : Restart three .power ⟨some two.visible.view.manifest, three.pages⟩
      (three.afterPower two.visible) :=
    .power _ _ (by
      change two.visible ∈ [one.confirmed, two.visible, three.visible]
      simp) (by exact ⟨[], rfl⟩)
  have read := (restart_from_empty 1 "json" exampleTrace crash).2
  change (reopen 1 "json" _).map _ = _
  rw [read]
  rfl

-- Confirming the recovered intermediate version makes it the only candidate.
example : ((three.afterPower two.visible).afterConfirm).candidates = [two.visible] := rfl

end PersistCollections.BPlusTree.Cow
