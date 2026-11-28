import Model

namespace PersistCollections.BPlusTree

inductive Page where
  | leaf (rows : List Row)
  | branch (children : List (Nat × Page))

abbrev Child := Nat × Page

/-- All rows visible from one root, in leaf order. Depth includes the root. -/
def flatten : Nat → Page → List Row
  | 0, _ => []
  | _ + 1, .leaf rows => rows
  | depth + 1, .branch children => children.flatMap (fun c => flatten depth c.2)

def orderedRows (rows : List Row) : Prop :=
  rows.Pairwise (fun a b => a.id < b.id)

def separated (depth : Nat) (a b : Child) : Prop :=
  a.1 < b.1 ∧ ∀ row ∈ flatten depth a.2, row.id < b.1

/-- Local fence invariants and enough depth to reach every leaf. -/
def Valid : Nat → Page → Prop
  | 0, _ => False
  | _ + 1, .leaf rows => orderedRows rows
  | depth + 1, .branch children =>
    children ≠ [] ∧ children.Pairwise (separated depth) ∧
      ∀ c ∈ children, Valid depth c.2 ∧ ∀ r ∈ flatten depth c.2, c.1 ≤ r.id

private theorem prefix_filters {α : Type} (xs : List α) (p : α → Bool)
    (ordered : xs.Pairwise (fun a b => p b = true → p a = true)) :
    xs.takeWhile p = xs.filter p ∧ xs.dropWhile p = xs.filter (fun a => !p a) := by
  induction xs with
  | nil => exact ⟨rfl, rfl⟩
  | cons a xs ih =>
    obtain ⟨head, tail⟩ := List.pairwise_cons.mp ordered
    cases h : p a with
    | true => simp [List.takeWhile, List.dropWhile, h, (ih tail).1, (ih tail).2]
    | false =>
      have miss : ∀ b ∈ xs, p b = false := by
        intro b member
        cases hb : p b
        · rfl
        · exact False.elim (Bool.false_ne_true (h.symm.trans (head b member hb)))
      have gone : xs.filter p = [] :=
        List.filter_eq_nil_iff.mpr (fun b hb => by simp [miss b hb])
      have keep : xs.filter (fun b => !p b) = xs :=
        List.filter_eq_self.mpr (fun b hb => by simp [miss b hb])
      simp [List.takeWhile, List.dropWhile, h, gone, keep]

private theorem leafScan_eq (rows : List Row) (q : Interval) (h : orderedRows rows) :
    (rows.dropWhile (fun r => decide (r.id < q.first))).takeWhile
      (fun r => decide (r.id ≤ q.last)) = rangeRows rows q := by
  rw [(prefix_filters _ _ (h.imp (by intro a b ab hit; simp at hit ⊢; omega))).2]
  rw [(prefix_filters _ _ ((h.filter _).imp (by intro a b ab hit; simp at hit ⊢; omega))).1]
  simp only [rangeRows, List.filter_filter]
  congr 1
  funext r
  by_cases lo : r.id < q.first
  · simp [Interval.contains, lo, show ¬ q.first ≤ r.id by omega]
  · simp [Interval.contains, lo, Nat.le_of_not_gt lo]

/-- On a partitioned leaf, dropping while is slicing at Rust's partition point. -/
theorem leaf_drop_eq_partition (rows : List Row) (lower : Nat) :
    rows.dropWhile (fun r => decide (r.id < lower)) =
      rows.drop ((rows.takeWhile (fun r => decide (r.id < lower))).length) := by
  induction rows with
  | nil => rfl
  | cons row rest ih =>
    by_cases h : row.id < lower <;> simp [List.takeWhile, List.dropWhile, h, ih]

/-- Seek to the last fence at or below lower, falling back to the first child. -/
def seek (lower : Nat) : List Child → List Child
  | [] => []
  | [a] => [a]
  | a :: b :: rest => if b.1 ≤ lower then seek lower (b :: rest) else a :: b :: rest

theorem seek_eq_partition (children : List Child) (lower : Nat)
    (sorted : children.Pairwise (fun a b => a.1 < b.1)) :
    seek lower children =
      children.drop ((children.takeWhile (fun c => decide (c.1 ≤ lower))).length - 1) := by
  induction children with
  | nil => rfl
  | cons a rest ih =>
    cases rest with
    | nil =>
      by_cases ha : a.1 ≤ lower <;> simp [seek, List.takeWhile, ha]
    | cons b rest =>
      obtain ⟨head, tail⟩ := List.pairwise_cons.mp sorted
      have ab := head b (by simp)
      by_cases hb : b.1 ≤ lower
      · have ha : a.1 ≤ lower := by omega
        rw [seek, if_pos hb, ih tail]
        simp [List.takeWhile, ha, hb]
      · by_cases ha : a.1 ≤ lower <;> simp [seek, List.takeWhile, ha, hb]

def window (q : Interval) (children : List Child) : List Child :=
  (seek q.first children).takeWhile (fun c => decide (c.1 ≤ q.last))

def visit (q : Interval) : Nat → Page → List Row
  | 0, _ => []
  | _ + 1, .leaf rows =>
    (rows.dropWhile (fun r => decide (r.id < q.first))).takeWhile
      (fun r => decide (r.id ≤ q.last))
  | depth + 1, .branch children =>
    (window q children).flatMap (fun c => visit q depth c.2)

private theorem range_flatMap (q : Interval) (rows : Child → List Row)
    (children : List Child) :
    rangeRows (children.flatMap rows) q = children.flatMap (fun c => rangeRows (rows c) q) := by
  induction children with
  | nil => rfl
  | cons a rest ih =>
    simpa only [List.flatMap_cons, rangeRows, List.filter_append] using
      congrArg (fun tail => rangeRows (rows a) q ++ tail) ih

private theorem window_keeps_hits (q : Interval) (depth : Nat) (children : List Child)
    (h : children.Pairwise (separated depth))
    (lower : ∀ c ∈ children, ∀ r ∈ flatten depth c.2, c.1 ≤ r.id) :
    (window q children).flatMap (fun c => rangeRows (flatten depth c.2) q) =
      children.flatMap (fun c => rangeRows (flatten depth c.2) q) := by
  have stop (xs : List Child) (sorted : xs.Pairwise (separated depth))
      (bounds : ∀ c ∈ xs, ∀ r ∈ flatten depth c.2, c.1 ≤ r.id) :
      (xs.takeWhile (fun c => decide (c.1 ≤ q.last))).flatMap
          (fun c => rangeRows (flatten depth c.2) q) =
        xs.flatMap (fun c => rangeRows (flatten depth c.2) q) := by
    rw [(prefix_filters _ _ (sorted.imp (by
      intro a b ab hit
      have := ab.1
      simp at hit ⊢
      omega))).1]
    induction xs with
    | nil => rfl
    | cons a rest ih =>
      have tail := ih (List.pairwise_cons.mp sorted).2
        (fun c hc => bounds c (List.mem_cons_of_mem a hc))
      by_cases ha : a.1 ≤ q.last
      · simp [ha, List.flatMap_cons, tail]
      · have gone : rangeRows (flatten depth a.2) q = [] := by
          apply List.filter_eq_nil_iff.mpr
          intro row member
          have bound := bounds a (by simp) row member
          simp [Interval.contains, show ¬ row.id ≤ q.last by omega]
        simp [ha, List.flatMap_cons, gone, tail]
  induction children with
  | nil => rfl
  | cons a rest ih =>
    cases rest with
    | nil => exact stop _ h lower
    | cons b rest =>
      obtain ⟨head, tail⟩ := List.pairwise_cons.mp h
      unfold window
      simp only [seek]
      split
      next hb =>
        have gone : rangeRows (flatten depth a.2) q = [] := by
          apply List.filter_eq_nil_iff.mpr
          intro row member
          have bound := (head b (by simp)).2 row member
          simp [Interval.contains, show ¬ q.first ≤ row.id by omega]
        rw [List.flatMap_cons, gone, List.nil_append]
        exact ih tail (fun c hc => lower c (List.mem_cons_of_mem a hc))
      next => exact stop _ h lower

/-- The inclusive scan equals filtering every row visible from the pinned root. -/
theorem visit_eq_rangeRows (q : Interval) (depth : Nat) (root : Page)
    (valid : Valid depth root) :
    visit q depth root = rangeRows (flatten depth root) q := by
  induction depth generalizing root with
  | zero => exact False.elim valid
  | succ depth ih =>
    cases root with
    | leaf rows => exact leafScan_eq rows q valid
    | branch children =>
      obtain ⟨_, fences, childValid⟩ := valid
      simp only [visit, flatten, range_flatMap]
      rw [← window_keeps_hits q depth children fences (fun c hc => (childValid c hc).2)]
      have agrees (c : Child) (hc : c ∈ window q children) :
          visit q depth c.2 = rangeRows (flatten depth c.2) q := by
        have kept := (List.takeWhile_prefix (l := seek q.first children)
          (fun c => decide (c.1 ≤ q.last))).sublist.subset hc
        rw [seek_eq_partition children q.first (fences.imp (fun h => h.1))] at kept
        exact ih c.2 (childValid c (List.mem_of_mem_drop kept)).1
      simp only [List.flatMap_def]
      exact congrArg List.flatten (List.map_congr_left agrees)

theorem flatten_ordered (depth : Nat) (root : Page) (valid : Valid depth root) :
    orderedRows (flatten depth root) := by
  induction depth generalizing root with
  | zero => exact False.elim valid
  | succ depth ih =>
    cases root with
    | leaf rows => exact valid
    | branch children =>
      obtain ⟨nonempty, fences, childValid⟩ := valid
      clear nonempty
      simp only [flatten]
      induction children with
      | nil => exact List.Pairwise.nil
      | cons a rest tail =>
        obtain ⟨head, orderedTail⟩ := List.pairwise_cons.mp fences
        have properties c (hc : c ∈ rest) := childValid c (List.mem_cons_of_mem a hc)
        simp only [List.flatMap_cons, orderedRows] at *
        apply List.pairwise_append.mpr
        refine ⟨ih a.2 (childValid a (by simp)).1, tail orderedTail properties, ?_⟩
        intro left hleft right hright
        obtain ⟨b, hb, hrow⟩ := List.mem_flatMap.mp hright
        exact Nat.lt_of_lt_of_le ((head b hb).2 left hleft) ((properties b hb).2 right hrow)

/-- The scan returns the same rows in the same strict ID order as the logical view. -/
theorem visit_same_view (q : Interval) (depth : Nat) (root : Page) (state : State)
    (valid : Valid depth root) (sameView : flatten depth root = state.rows) :
    visit q depth root = rangeRows state.rows q ∧ orderedRows (visit q depth root) := by
  rw [visit_eq_rangeRows q depth root valid]
  exact ⟨congrArg (fun rows => rangeRows rows q) sameView, (flatten_ordered depth root valid).filter _⟩

theorem visit_reversed (q : Interval) (depth : Nat) (root : Page)
    (valid : Valid depth root) (reversed : q.last < q.first) :
    visit q depth root = [] := by
  rw [visit_eq_rangeRows q depth root valid]
  apply List.filter_eq_nil_iff.mpr
  intro row _
  simp only [Interval.contains, decide_eq_true_eq]
  omega

/-- Public entry point: no root or a reversed interval means no page visits. -/
def scan (q : Interval) (depth : Nat) (root : Option Page) : List Row :=
  if q.first ≤ q.last then root.elim [] (visit q depth) else []

theorem scan_eq_rangeRows (q : Interval) (depth : Nat) (root : Option Page)
    (valid : ∀ page, root = some page → Valid depth page) :
    scan q depth root = rangeRows (root.elim [] (flatten depth)) q := by
  cases root with
  | none =>
    by_cases bounds : q.first ≤ q.last <;> simp [scan, bounds, rangeRows]
  | some page =>
    have pageValid := valid page rfl
    by_cases bounds : q.first ≤ q.last
    · simpa [scan, bounds] using visit_eq_rangeRows q depth page pageValid
    · have reversed := visit_reversed q depth page pageValid (Nat.lt_of_not_ge bounds)
      have spec := visit_eq_rangeRows q depth page pageValid
      simpa [scan, bounds] using reversed.symm.trans spec

theorem restrict_visit (depth : Nat) (root : Page) (state : State) (predicate : Predicate)
    (valid : Valid depth root) (sameView : flatten depth root = state.rows)
    (stateValid : state.Valid) :
    restrict (visit predicate.bounds depth root) predicate = restrict state.rows predicate :=
  bplus_restrict state stateValid predicate (fun q => visit q depth root)
    (fun q => (visit_same_view q depth root state valid sameView).1)

end PersistCollections.BPlusTree
