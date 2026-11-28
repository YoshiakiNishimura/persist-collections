import Lean

namespace PersistCollections

def maxId : Nat := 2 ^ 64 - 1

structure Row where
  id : Nat
  text : String
  deriving DecidableEq, Repr

structure State where
  rows : List Row
  nextId : Nat
  deriving DecidableEq, Repr

def empty : State := ⟨[], 1⟩

def State.Valid (state : State) : Prop :=
  1 ≤ state.nextId ∧ state.nextId ≤ maxId ∧
  state.rows.Pairwise (fun left right => left.id < right.id) ∧
  ∀ row ∈ state.rows, 1 ≤ row.id ∧ row.id < state.nextId

theorem empty_valid : empty.Valid := by
  simp [State.Valid, empty, maxId]

inductive Change where
  | insert (id : Nat) (text : String) (nextId : Nat)
  | delete (id : Nat)
  deriving Repr

/-- Apply an explicit change. Replay uses the recorded ID rather than allocating again. -/
def apply (state : State) (change : Change) : Option State :=
  match change with
  | .insert id text nextId =>
      if id = state.nextId ∧ nextId = id + 1 ∧ nextId ≤ maxId then
        some ⟨state.rows ++ [⟨id, text⟩], nextId⟩
      else none
  | .delete id =>
      if state.rows.any (fun row => row.id == id) then
        some ⟨state.rows.filter (fun row => row.id != id), state.nextId⟩
      else none

/-- Successful updates preserve order, unique IDs and the allocation bounds. -/
theorem apply_preserves_valid (state next : State) (change : Change)
    (valid : state.Valid) (applied : apply state change = some next) : next.Valid := by
  rcases valid with ⟨positive, bounded, ordered, rowBounds⟩
  cases change with
  | insert id text nextId =>
    simp only [apply] at applied
    split at applied
    next allowed =>
      rcases allowed with ⟨rfl, rfl, upper⟩
      cases applied
      refine ⟨by simp +arith, upper, List.pairwise_append.mpr ⟨ordered, by simp, ?_⟩, ?_⟩
      · intro left member right singleton
        simp only [List.mem_singleton] at singleton
        subst right
        exact (rowBounds left member).2
      · intro row member
        rcases List.mem_append.mp member with old | new
        · obtain ⟨lo, hi⟩ := rowBounds row old
          exact ⟨lo, Nat.lt_succ_of_lt hi⟩
        · simp only [List.mem_singleton] at new
          subst row
          exact ⟨positive, Nat.lt_succ_self _⟩
    next => contradiction
  | delete id =>
    simp only [apply] at applied
    split at applied
    next =>
      cases applied
      exact ⟨positive, bounded, ordered.filter _, fun row member =>
        rowBounds row (List.mem_filter.mp member).1⟩
    next => contradiction

structure Interval where
  first : Nat
  last : Nat
  deriving DecidableEq, Repr

def Interval.contains (interval : Interval) (id : Nat) : Bool :=
  decide (interval.first ≤ id ∧ id ≤ interval.last)

def Interval.intersect (left right : Interval) : Interval :=
  ⟨max left.first right.first, min left.last right.last⟩

inductive Predicate where
  | idBetween (range : Interval)
  | textEq (text : String)
  | and (left right : Predicate)
  deriving Repr

def Predicate.eval : Predicate → Row → Bool
  | .idBetween range, row => range.contains row.id
  | .textEq text, row => row.text == text
  | .and left right, row => left.eval row && right.eval row

def Predicate.bounds : Predicate → Interval
  | .idBetween range => range
  | .textEq _ => ⟨1, maxId⟩
  | .and left right => left.bounds.intersect right.bounds

def restrict (rows : List Row) (predicate : Predicate) : List Row :=
  rows.filter predicate.eval

def rangeRows (rows : List Row) (interval : Interval) : List Row :=
  rows.filter (fun row => interval.contains row.id)

/-- Intersection is equivalent to applying the restrictions successively. -/
theorem restrict_and (rows : List Row) (left right : Predicate) :
    restrict rows (.and left right) = restrict (restrict rows left) right := by
  simp [restrict, Predicate.eval, List.filter_filter, Bool.and_comm]

/-- The extracted interval contains every matching valid ID. -/
theorem bounds_sound (predicate : Predicate) (row : Row)
    (validId : 1 ≤ row.id ∧ row.id ≤ maxId)
    (hit : predicate.eval row = true) :
    predicate.bounds.contains row.id = true := by
  induction predicate with
  | idBetween interval => exact hit
  | textEq expected => simpa [Predicate.bounds, Interval.contains] using validId
  | and left right ihLeft ihRight =>
    have both : left.eval row = true ∧ right.eval row = true := by
      simpa [Predicate.eval] using hit
    have hl := ihLeft both.1
    have hr := ihRight both.2
    simp only [Interval.contains, decide_eq_true_eq] at hl hr
    simp only [Predicate.bounds, Interval.intersect, Interval.contains, decide_eq_true_eq]
    exact ⟨Nat.max_le.mpr ⟨hl.1, hr.1⟩, Nat.le_min.mpr ⟨hl.2, hr.2⟩⟩

private theorem filter_pushdown (rows : List Row) (test candidate : Row → Bool)
    (sound : ∀ row ∈ rows, test row = true → candidate row = true) :
    (rows.filter candidate).filter test = rows.filter test := by
  induction rows with
  | nil => rfl
  | cons row rows ih =>
    have tail := ih (fun other member => sound other (List.mem_cons_of_mem _ member))
    cases ht : test row with
    | false =>
      cases hc : candidate row <;> simp [List.filter, ht, hc, tail]
    | true =>
      have hc := sound row (by simp) ht
      simp [List.filter, ht, hc, tail]

/-- Restricting to the candidate interval cannot change values, multiplicity or order. -/
theorem range_pushdown (state : State) (valid : state.Valid) (predicate : Predicate) :
    restrict (rangeRows state.rows predicate.bounds) predicate =
      restrict state.rows predicate := by
  apply filter_pushdown
  intro row member hit
  have idBound := valid.2.2.2 row member
  exact bounds_sound predicate row ⟨idBound.1, Nat.le_trans (Nat.le_of_lt idBound.2) valid.2.1⟩ hit

/-- A B+ tree route can be substituted for the reference range scan when it
    returns exactly the rows in range, in ID order, from the same view. -/
theorem bplus_restrict (state : State) (valid : state.Valid)
    (predicate : Predicate) (visit : Interval → List Row)
    (visit_correct : ∀ interval, visit interval = rangeRows state.rows interval) :
    restrict (visit predicate.bounds) predicate = restrict state.rows predicate := by
  rw [visit_correct]
  exact range_pushdown state valid predicate

example : (apply empty (.insert 1 "note" 2)).bind (fun state =>
    apply state (.delete 1)) = some ⟨[], 2⟩ := by decide

example : apply empty (.delete 1) = none := by decide

end PersistCollections
