import BPlusTree

namespace PersistCollections.BPlusTree

inductive Edit where
  | insert (row : Row)
  | delete (id : Nat)

def Edit.key : Edit → Nat
  | .insert row => row.id
  | .delete id => id

/-- Successful edits affect only the requested key and preserve all other rows. -/
def Edit.Contains (edit : Edit) (old : List Row) (r : Row) : Prop :=
  match edit with
  | .insert row => r ∈ old ∨ r = row
  | .delete id => r ∈ old ∧ r.id ≠ id

def insertRow (row : Row) : List Row → Option (List Row)
  | [] => some [row]
  | r :: rs =>
    if row.id < r.id then some (row :: r :: rs)
    else if row.id = r.id then none
    else (insertRow row rs).map (r :: ·)

def editRows (edit : Edit) (rows : List Row) : Option (List Row) :=
  match edit with
  | .insert row => insertRow row rows
  | .delete id =>
    if rows.any (fun r => r.id == id) then some (rows.filter (fun r => r.id != id))
    else none

def fanout : Nat := 16

/-- An insert splits at the same midpoint as Rust. Delete never splits or merges. -/
def chunks (edit : Edit) (xs : List α) : List (List α) :=
  if xs.isEmpty then []
  else match edit with
    | .delete _ => [xs]
    | .insert _ =>
      if fanout < xs.length then [xs.take (xs.length / 2), xs.drop (xs.length / 2)]
      else [xs]

private theorem chunks_flatten (edit : Edit) (xs : List α) :
    (chunks edit xs).flatten = xs := by
  cases xs with
  | nil => rfl
  | cons a rest =>
    cases edit with
    | delete => simp [chunks]
    | insert => by_cases h : fanout < rest.length + 1 <;> simp [chunks, h]

private theorem chunks_nonempty (edit : Edit) (xs part : List α)
    (h : part ∈ chunks edit xs) : part ≠ [] := by
  cases xs with
  | nil => simp [chunks] at h
  | cons a rest =>
    cases edit with
    | delete =>
      have eq : part = a :: rest := by simpa [chunks] using h
      simp [eq]
    | insert =>
      by_cases big : fanout < rest.length + 1
      · simp [chunks, big] at h
        have n : 2 ≤ rest.length + 1 := by unfold fanout at big; omega
        rcases h with rfl | rfl <;>
          intro empty <;> have len := congrArg List.length empty <;> simp at len n <;> omega
      · have eq : part = a :: rest := by simpa [chunks, big] using h
        simp [eq]

def childRows (depth : Nat) (children : List Child) : List Row :=
  children.flatMap (fun c => flatten depth c.2)

/-- Strengthen the scan invariant with exact, nonempty first keys. -/
def Exact : Nat → Child → Prop
  | 0, _ => False
  | depth + 1, (first, page) =>
    (∃ r rest, flatten (depth + 1) page = r :: rest ∧ r.id = first) ∧
    orderedRows (flatten (depth + 1) page) ∧
    match page with
    | .leaf _ => True
    | .branch children => ∀ c ∈ children, Exact depth c

def Forest (depth : Nat) (children : List Child) : Prop :=
  orderedRows (childRows depth children) ∧ ∀ c ∈ children, Exact depth c

private theorem exact_head (h : Exact depth c) :
    ∃ r rest, flatten depth c.2 = r :: rest ∧ r.id = c.1 := by
  cases depth with
  | zero => exact False.elim h
  | succ d => exact h.1

private theorem exact_ordered (h : Exact depth c) : orderedRows (flatten depth c.2) := by
  cases depth with
  | zero => exact False.elim h
  | succ d => exact h.2.1

private theorem fence_le (h : Exact depth c) (member : r ∈ flatten depth c.2) : c.1 ≤ r.id := by
  obtain ⟨first, rest, rows, id⟩ := exact_head h
  have ordered := exact_ordered h
  rw [rows] at member ordered
  rcases List.mem_cons.mp member with eq | tail
  · subst r
    omega
  · have := (List.pairwise_cons.mp ordered).1 r tail
    omega

private theorem forest_cons :
    Forest depth (c :: cs) ↔ Forest depth [c] ∧ Forest depth cs ∧
      (∀ a ∈ flatten depth c.2, ∀ b ∈ childRows depth cs, a.id < b.id) := by
  simp only [Forest, childRows, List.flatMap_cons, List.flatMap_nil, List.append_nil,
    orderedRows, List.pairwise_append, List.mem_cons, List.mem_nil_iff, or_false]
  constructor
  · rintro ⟨⟨a, b, across⟩, each⟩
    exact ⟨⟨a, fun x eq => each x (Or.inl eq)⟩, ⟨b, fun x hx => each x (Or.inr hx)⟩, across⟩
  · rintro ⟨⟨a, one⟩, ⟨b, each⟩, across⟩
    exact ⟨⟨a, b, across⟩, fun x hx => hx.elim (one x) (each x)⟩

private theorem forest_append :
    Forest depth (cs ++ ds) ↔ Forest depth cs ∧ Forest depth ds ∧
      (∀ a ∈ childRows depth cs, ∀ b ∈ childRows depth ds, a.id < b.id) := by
  simp only [Forest, childRows, List.flatMap_append, orderedRows, List.pairwise_append,
    List.mem_append]
  constructor
  · rintro ⟨⟨a, b, across⟩, each⟩
    exact ⟨⟨a, fun x hx => each x (Or.inl hx)⟩, ⟨b, fun x hx => each x (Or.inr hx)⟩, across⟩
  · rintro ⟨⟨a, left⟩, ⟨b, right⟩, across⟩
    exact ⟨⟨a, b, across⟩, fun x hx => hx.elim (left x) (right x)⟩

private theorem forest_single (h : Exact depth c) : Forest depth [c] := by
  simpa [Forest, childRows] using And.intro (exact_ordered h) h

private theorem forest_fences (h : Forest depth cs) : cs.Pairwise (separated depth) := by
  induction cs with
  | nil => exact .nil
  | cons c cs ih =>
    obtain ⟨_, tail, across⟩ := forest_cons.mp h
    apply List.pairwise_cons.mpr
    refine ⟨?_, ih tail⟩
    intro b hb
    obtain ⟨r, rs, rows, id⟩ := exact_head (tail.2 b hb)
    have hr : r ∈ childRows depth cs :=
      List.mem_flatMap.mpr ⟨b, hb, by simp [rows]⟩
    have bound a (ha : a ∈ flatten depth c.2) : a.id < b.1 := by
      rw [← id]
      exact across a ha r hr
    obtain ⟨a, as, ar, aid⟩ := exact_head (h.2 c (by simp))
    exact ⟨aid ▸ bound a (by simp [ar]), bound⟩

theorem exact_valid (depth : Nat) (c : Child) (h : Exact depth c) : Valid depth c.2 := by
  induction depth generalizing c with
  | zero => exact False.elim h
  | succ depth ih =>
    rcases c with ⟨first, page⟩
    cases page with
    | leaf rows => exact h.2.1
    | branch children =>
      have forest : Forest depth children := ⟨h.2.1, h.2.2⟩
      obtain ⟨r, rs, nonempty, _⟩ := exact_head h
      refine ⟨?_, forest_fences forest, fun child hc =>
        ⟨ih child (forest.2 child hc), fun _ hr => fence_le (forest.2 child hc) hr⟩⟩
      intro eq
      simp [eq, flatten] at nonempty

private theorem insertRow_spec (row : Row) (xs ys : List Row)
    (ordered : orderedRows xs) (result : insertRow row xs = some ys) :
    orderedRows ys ∧ ∀ r, r ∈ ys ↔ r ∈ xs ∨ r = row := by
  induction xs generalizing ys with
  | nil =>
    simp [insertRow] at result
    subst ys
    simp [orderedRows]
  | cons a rest ih =>
    obtain ⟨head, tail⟩ := List.pairwise_cons.mp ordered
    simp only [insertRow] at result
    split at result
    next lt =>
      cases result
      refine ⟨List.pairwise_cons.mpr ⟨?_, ordered⟩, by simp [or_comm, or_left_comm]⟩
      intro r hr
      rcases List.mem_cons.mp hr with eq | mem
      · simpa [eq] using lt
      · exact Nat.lt_trans lt (head r mem)
    next notLt =>
      split at result
      · contradiction
      next notEq =>
        cases h : insertRow row rest with
        | none => simp [h] at result
        | some new =>
          simp [h] at result
          subst ys
          obtain ⟨ord, mem⟩ := ih new tail h
          refine ⟨List.pairwise_cons.mpr ⟨?_, ord⟩, ?_⟩
          · intro r hr
            rcases (mem r).mp hr with old | eq
            · exact head r old
            · subst r
              omega
          · intro r
            simp only [List.mem_cons, mem, or_assoc]

private theorem editRows_spec (edit : Edit) (xs ys : List Row)
    (ordered : orderedRows xs) (result : editRows edit xs = some ys) :
    orderedRows ys ∧ ∀ r, r ∈ ys ↔ edit.Contains xs r := by
  cases edit with
  | insert row => exact insertRow_spec row xs ys ordered result
  | delete id =>
    simp only [editRows] at result
    split at result
    · cases result
      exact ⟨ordered.filter _, fun r => by simp [Edit.Contains]⟩
    · contradiction

def editChildren (key : Nat) (update : Child → Option (List Child)) :
    List Child → Option (List Child)
  | [] => none
  | [a] => update a
  | a :: b :: rest =>
    if b.1 ≤ key then (editChildren key update (b :: rest)).map (a :: ·)
    else (update a).map (· ++ b :: rest)

/-- The recursive rewrite splices into exactly Rust's predecessor slot. -/
theorem editChildren_eq_partition (key : Nat) (update : Child → Option (List Child))
    (cs : List Child) (sorted : cs.Pairwise (fun a b => a.1 < b.1)) :
    editChildren key update cs =
      let slot := (cs.takeWhile (fun c => decide (c.1 ≤ key))).length - 1
      match cs.drop slot with
      | [] => none
      | c :: rest => (update c).map (fun out => cs.take slot ++ out ++ rest) := by
  induction cs with
  | nil => rfl
  | cons a cs ih =>
    cases cs with
    | nil => by_cases h : a.1 ≤ key <;> simp [editChildren, List.takeWhile, h]
    | cons b rest =>
      obtain ⟨head, tail⟩ := List.pairwise_cons.mp sorted
      have ab := head b (by simp)
      by_cases hb : b.1 ≤ key
      · have ha : a.1 ≤ key := by omega
        simp only [editChildren, if_pos hb]
        rw [ih tail]
        simp only [List.takeWhile, ha, hb, decide_true, if_true, List.length_cons]
        simp only [Nat.add_sub_cancel, List.drop_succ_cons, List.take_succ_cons]
        cases List.drop (List.takeWhile (fun c => decide (c.1 ≤ key)) rest).length (b :: rest) with
        | nil => rfl
        | cons c suffix => cases h : update c <;> simp [h]
      · by_cases ha : a.1 ≤ key <;> simp [editChildren, List.takeWhile, ha, hb]

private theorem left_mem (edit : Edit) (xs ys : List Row) (r : Row)
    (away : ∀ a ∈ ys, a.id ≠ edit.key) :
    edit.Contains (xs ++ ys) r ↔ edit.Contains xs r ∨ r ∈ ys := by
  cases edit with
  | insert row => simp [Edit.Contains, or_right_comm]
  | delete id =>
    simp only [Edit.Contains, List.mem_append]
    constructor
    · rintro ⟨h | h, ne⟩
      · exact Or.inl ⟨h, ne⟩
      · exact Or.inr h
    · rintro (⟨h, ne⟩ | h)
      · exact ⟨Or.inl h, ne⟩
      · exact ⟨Or.inr h, away r h⟩

private theorem right_mem (edit : Edit) (xs ys : List Row) (r : Row)
    (away : ∀ a ∈ xs, a.id ≠ edit.key) :
    edit.Contains (xs ++ ys) r ↔ r ∈ xs ∨ edit.Contains ys r := by
  rw [show edit.Contains (xs ++ ys) r ↔ edit.Contains (ys ++ xs) r by
    cases edit <;> simp [Edit.Contains, or_comm]]
  rw [left_mem edit ys xs r away, or_comm]

private theorem old_or_key (edit : Edit) {old : List Row} {r : Row}
    (h : edit.Contains old r) : r ∈ old ∨ r.id = edit.key := by
  cases edit with
  | insert row => exact h.elim Or.inl (fun eq => Or.inr (congrArg Row.id eq))
  | delete id => exact Or.inl h.1

private theorem editChildren_spec (edit : Edit) (depth : Nat)
    (update : Child → Option (List Child))
    (preserves : ∀ c out, Exact depth c → update c = some out →
      Forest depth out ∧ ∀ r, r ∈ childRows depth out ↔ edit.Contains (flatten depth c.2) r)
    (cs out : List Child) (valid : Forest depth cs)
    (result : editChildren edit.key update cs = some out) :
    Forest depth out ∧ ∀ r, r ∈ childRows depth out ↔ edit.Contains (childRows depth cs) r := by
  induction cs generalizing out with
  | nil => contradiction
  | cons a tail ih =>
    cases tail with
    | nil =>
      simpa [childRows] using preserves a out (valid.2 a (by simp)) result
    | cons b rest =>
      obtain ⟨one, others, across⟩ := forest_cons.mp valid
      have ab := (List.pairwise_cons.mp (forest_fences valid)).1 b (by simp)
      simp only [editChildren] at result
      split at result
      next low =>
        cases eq : editChildren edit.key update (b :: rest) with
        | none => simp [eq] at result
        | some new =>
          simp [eq] at result
          subst out
          obtain ⟨newForest, mem⟩ := ih new others eq
          have before r (hr : r ∈ flatten depth a.2) : r.id < edit.key :=
            Nat.lt_of_lt_of_le (ab.2 r hr) low
          constructor
          · apply forest_cons.mpr
            refine ⟨one, newForest, ?_⟩
            intro x hx y hy
            rcases old_or_key edit ((mem y).mp hy) with old | key
            · exact across x hx y old
            · rw [key]
              exact before x hx
          · intro r
            change (r ∈ flatten depth a.2 ++ childRows depth new) ↔
              edit.Contains (flatten depth a.2 ++ childRows depth (b :: rest)) r
            rw [List.mem_append, mem, right_mem edit _ _ _ (fun x hx => Nat.ne_of_lt (before x hx))]
      next high =>
        cases eq : update a with
        | none => simp [eq] at result
        | some new =>
          simp [eq] at result
          subst out
          obtain ⟨newForest, mem⟩ := preserves a new (valid.2 a (by simp)) eq
          have after r (hr : r ∈ childRows depth (b :: rest)) : edit.key < r.id := by
            obtain ⟨c, hc, member⟩ := List.mem_flatMap.mp hr
            have bound := fence_le (others.2 c hc) member
            rcases List.mem_cons.mp hc with eq | later
            · subst c
              omega
            · have bc := ((List.pairwise_cons.mp (forest_fences others)).1 c later).1
              omega
          constructor
          · apply forest_append.mpr
            refine ⟨newForest, others, ?_⟩
            intro x hx y hy
            rcases old_or_key edit ((mem x).mp hx) with old | key
            · exact across x old y hy
            · rw [key]
              exact after y hy
          · intro r
            rw [show childRows depth (new ++ b :: rest) =
              childRows depth new ++ childRows depth (b :: rest) from List.flatMap_append]
            change (r ∈ childRows depth new ++ childRows depth (b :: rest)) ↔
              edit.Contains (flatten depth a.2 ++ childRows depth (b :: rest)) r
            rw [List.mem_append, mem, left_mem edit _ _ _ (fun x hx => Nat.ne_of_gt (after x hx))]

def leafLink (rows : List Row) : Child :=
  ((rows.head?.map Row.id).getD 0, .leaf rows)

def branchLink (children : List Child) : Child :=
  ((children.head?.map Prod.fst).getD 0, .branch children)

private theorem pack_spec (depth : Nat) (edit : Edit) (items : List α)
    (rows : α → List Row) (link : List α → Child)
    (ordered : orderedRows (items.flatMap rows))
    (flattenLink : ∀ xs, flatten depth (link xs).2 = xs.flatMap rows)
    (exactLink : ∀ xs ∈ chunks edit items, Exact depth (link xs)) :
    Forest depth ((chunks edit items).map link) ∧
      childRows depth ((chunks edit items).map link) = items.flatMap rows := by
  have flat : childRows depth ((chunks edit items).map link) = items.flatMap rows := by
    simp only [childRows, List.flatMap_map, Function.comp_def, flattenLink]
    rw [← List.flatMap_assoc]
    change ((chunks edit items).flatMap id).flatMap rows = _
    rw [List.flatMap_id, chunks_flatten]
  refine ⟨⟨flat ▸ ordered, ?_⟩, flat⟩
  intro c hc
  obtain ⟨xs, hxs, rfl⟩ := List.mem_map.mp hc
  exact exactLink xs hxs

private theorem pack_leaves (depth : Nat) (edit : Edit) (rows : List Row)
    (ordered : orderedRows rows) :
    Forest (depth + 1) ((chunks edit rows).map leafLink) ∧
      childRows (depth + 1) ((chunks edit rows).map leafLink) = rows := by
  have singletonFlat (xs : List Row) : xs.flatMap (fun r => [r]) = xs := by
    induction xs <;> simp [*]
  simpa only [singletonFlat] using
    pack_spec (depth + 1) edit rows (fun r => [r]) leafLink
      (by simpa only [singletonFlat] using ordered)
      (fun xs => by simp [leafLink, flatten, singletonFlat])
      (by
        intro xs hx
        have sub := List.sublist_flatten_of_mem hx
        rw [chunks_flatten] at sub
        have ord := ordered.sublist sub
        cases xs with
        | nil => exact False.elim (chunks_nonempty edit rows [] hx rfl)
        | cons r rest => exact ⟨⟨r, rest, rfl, rfl⟩, ord, trivial⟩)

private theorem branch_exact (depth : Nat) (cs : List Child)
    (nonempty : cs ≠ []) (forest : Forest depth cs) :
    Exact (depth + 1) (branchLink cs) := by
  cases cs with
  | nil => contradiction
  | cons a rest =>
    obtain ⟨r, rs, head, id⟩ := exact_head (forest.2 a (by simp))
    exact ⟨⟨r, rs ++ childRows depth rest, by simp [flatten, List.flatMap_cons, head, childRows],
      id⟩, forest.1, forest.2⟩

private theorem pack_branches (depth : Nat) (edit : Edit) (cs : List Child)
    (forest : Forest depth cs) :
    Forest (depth + 1) ((chunks edit cs).map branchLink) ∧
      childRows (depth + 1) ((chunks edit cs).map branchLink) = childRows depth cs := by
  refine pack_spec (depth + 1) edit cs (fun c => flatten depth c.2) branchLink
    forest.1 (fun _ => rfl) ?_
  intro xs hx
  have sub := List.sublist_flatten_of_mem hx
  rw [chunks_flatten] at sub
  apply branch_exact depth xs (chunks_nonempty edit cs xs hx)
  have rowsSub : List.Sublist (childRows depth xs) (childRows depth cs) := by
    clear forest hx
    induction sub with
    | slnil => exact List.Sublist.slnil
    | cons a _ ih =>
      exact ih.trans (List.sublist_append_right _ _)
    | cons₂ a _ ih =>
      exact ih.append_left (flatten depth a.2)
  exact ⟨forest.1.sublist rowsSub, fun c hc => forest.2 c (sub.subset hc)⟩

/-- A recursive edit returns zero, one, or two replacement links. -/
def updatePage (edit : Edit) : Nat → Page → Option (List Child)
  | 0, _ => none
  | _ + 1, .leaf rows =>
    (editRows edit rows).map (fun new => (chunks edit new).map leafLink)
  | depth + 1, .branch children =>
    (editChildren edit.key (fun c => updatePage edit depth c.2) children).map
      (fun new => (chunks edit new).map branchLink)

theorem updatePage_spec (edit : Edit) (depth : Nat) (link : Child) (out : List Child)
    (valid : Exact depth link) (result : updatePage edit depth link.2 = some out) :
    Forest depth out ∧ ∀ r, r ∈ childRows depth out ↔ edit.Contains (flatten depth link.2) r := by
  induction depth generalizing link out with
  | zero => exact False.elim valid
  | succ depth ih =>
    rcases link with ⟨first, page⟩
    cases page with
    | leaf rows =>
      cases eq : editRows edit rows with
      | none => simp [updatePage, eq] at result
      | some new =>
        simp [updatePage, eq] at result
        subst out
        obtain ⟨ordered, mem⟩ := editRows_spec edit rows new valid.2.1 eq
        obtain ⟨forest, flat⟩ := pack_leaves depth edit new ordered
        exact ⟨forest, fun r => by rw [flat]; exact mem r⟩
    | branch cs =>
      cases eq : editChildren edit.key (fun c => updatePage edit depth c.2) cs with
      | none => simp [updatePage, eq] at result
      | some new =>
        simp [updatePage, eq] at result
        subst out
        obtain ⟨children, mem⟩ := editChildren_spec edit depth _ ih cs new ⟨valid.2.1, valid.2.2⟩ eq
        obtain ⟨forest, flat⟩ := pack_branches depth edit new children
        exact ⟨forest, fun r => by rw [flat]; exact mem r⟩

structure Root where
  depth : Nat
  link : Option Child

def Root.rows (root : Root) : List Row :=
  root.link.elim [] (fun c => flatten root.depth c.2)

def Root.Exact (root : Root) : Prop :=
  ∀ c ∈ root.link, BPlusTree.Exact root.depth c

def finish (edit : Edit) (depth : Nat) : List Child → Root
  | [] => ⟨0, none⟩
  | [c] =>
    match edit, depth, c.2 with
    | .delete _, d + 1, .branch [child] => ⟨d, some child⟩
    | _, _, _ => ⟨depth, some c⟩
  | a :: b :: rest => ⟨depth + 1, some (branchLink (a :: b :: rest))⟩

def updateRoot (edit : Edit) (root : Root) : Option Root :=
  match root.link with
  | none => match edit with
    | .delete _ => none
    | .insert r => some ⟨1, some (leafLink [r])⟩
  | some c => (updatePage edit root.depth c.2).map (finish edit root.depth)

private theorem finish_spec (edit : Edit) (depth : Nat) (links : List Child)
    (forest : Forest depth links) :
    (finish edit depth links).Exact ∧ (finish edit depth links).rows = childRows depth links := by
  cases links with
  | nil => simp [finish, Root.Exact, Root.rows, childRows]
  | cons a tail =>
    cases tail with
    | cons b rest =>
      exact ⟨fun c hc => by
        have eq : branchLink (a :: b :: rest) = c := by simpa [finish] using hc
        subst c
        exact branch_exact depth _ (by simp) forest, rfl⟩
    | nil =>
      have exactA := forest.2 a (by simp)
      cases edit <;> cases depth with
      | zero => exact False.elim exactA
      | succ d =>
        rcases a with ⟨f, page⟩
        cases page with
        | leaf rows => simpa [finish, Root.Exact, Root.rows, childRows] using exactA
        | branch cs =>
          cases cs with
          | nil => simpa [finish, Root.Exact, Root.rows, childRows] using exactA
          | cons c cs =>
            cases cs with
            | nil =>
              first
              | simpa [finish, Root.Exact, Root.rows, childRows] using exactA
              | have child := exactA.2.2 c (by simp)
                simpa [finish, Root.Exact, Root.rows, childRows, flatten] using child
            | cons b rest => simpa [finish, Root.Exact, Root.rows, childRows] using exactA

theorem updateRoot_spec (edit : Edit) (root next : Root)
    (valid : root.Exact) (result : updateRoot edit root = some next) :
    next.Exact ∧ ∀ r, r ∈ next.rows ↔ edit.Contains root.rows r := by
  rcases root with ⟨depth, root⟩
  cases root with
  | none =>
    cases edit with
    | delete => contradiction
    | insert row =>
      cases result
      constructor
      · intro c hc
        have eq : leafLink [row] = c := by simpa using hc
        subst c
        exact ⟨⟨row, [], rfl, rfl⟩, by simp [orderedRows, flatten, leafLink], trivial⟩
      · simp [Root.rows, Edit.Contains, leafLink, flatten]
  | some c =>
    cases eq : updatePage edit depth c.2 with
    | none => simp [updateRoot, eq] at result
    | some links =>
      simp [updateRoot, eq] at result
      subst next
      obtain ⟨forest, mem⟩ := updatePage_spec edit depth c links (valid c (by simp)) eq
      obtain ⟨shape, flat⟩ := finish_spec edit depth links forest
      exact ⟨shape, fun r => by rw [flat]; exact mem r⟩

/-- Every published model root after a successful edit meets the range-scan contract. -/
theorem scan_after_update (edit : Edit) (root next : Root) (q : Interval)
    (valid : root.Exact) (result : updateRoot edit root = some next) :
    scan q next.depth (next.link.map Prod.snd) = rangeRows next.rows q ∧
      orderedRows (scan q next.depth (next.link.map Prod.snd)) := by
  have inv := (updateRoot_spec edit root next valid result).1
  rcases next with ⟨depth, link⟩
  cases link with
  | none =>
    have eq := scan_eq_rangeRows q depth none (by simp)
    exact ⟨eq, by change orderedRows (scan q depth none); rw [eq]; exact List.Pairwise.nil⟩
  | some c =>
    have localValid := exact_valid depth c (inv c (by simp))
    have eq := scan_eq_rangeRows q depth (some c.2) (by
      intro p hp
      cases hp
      exact localValid)
    exact ⟨eq, by
      change orderedRows (scan q depth (some c.2))
      rw [eq]
      exact (exact_ordered (inv c (by simp))).filter _⟩

private def sampleRow (id : Nat) : Row := ⟨id, ""⟩

-- Seventeen rows split into pages of eight and nine, and a new root is added.
example :
    let root : Root := ⟨1, some (leafLink ((List.range 16).map (sampleRow ∘ Nat.succ)))⟩
    let summary (next : Root) := (next.depth, next.link.map (fun c => match c.2 with
        | .leaf _ => []
        | .branch children => children.map (fun child => (child.1, (flatten 1 child.2).length))))
    (updateRoot (.insert (sampleRow 0)) root).map summary =
      some (2, some [(0, 8), (8, 9)]) := by decide

-- A full branch splits when its last leaf splits.
example :
    let children := (List.range 15).map (fun i => leafLink [sampleRow (i + 1)])
    let last := leafLink ((List.range 16).map (fun i => sampleRow (i + 16)))
    let root : Root := ⟨2, some (branchLink (children ++ [last]))⟩
    let summary (next : Root) := (next.depth, next.link.map (fun c => match c.2 with
      | .leaf _ => []
      | .branch links => links.map Prod.fst))
    (updateRoot (.insert (sampleRow 32)) root).map summary = some (3, some [1, 9]) := by decide

-- Removing the first singleton leaf exposes the other leaf as the root.
example :
    let root : Root := ⟨2, some (branchLink [leafLink [sampleRow 1], leafLink [sampleRow 4]])⟩
    (updateRoot (.delete 1) root).map (fun next =>
      (next.depth, next.link.map Prod.fst, next.rows.map Row.id)) =
      some (1, some 4, [4]) := by decide

end PersistCollections.BPlusTree
